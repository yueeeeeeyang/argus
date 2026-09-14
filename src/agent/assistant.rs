//! 文件职责：交互助手的薄装配层。
//! 创建日期：2026-07-16
//! 修改日期：2026-09-14
//! 作者：Argus 开发团队
//! 主要功能：把中立历史和当前问题组装为通用智能体循环请求；模型循环本身由 agent_loop 提供。

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use rig_core::completion::Message;
use secrecy::SecretString;

use crate::agent::agent_loop::{AgentLoopNote, AgentLoopRequest, run_agent_loop};
use crate::agent::session::SourceScopeSnapshot;
use crate::config::{AiConfig, AiModelProfile};

/// 一轮已经完成的中立对话记录；不保存 Provider 工具协议、原始工具结果或思考过程。
#[derive(Clone, Debug)]
pub(crate) struct AssistantHistoryTurn {
    /// 本轮实际进入模型上下文的用户消息，保持可见顺序。
    pub user_messages: Vec<String>,
    /// 模型面向用户的最终可见正文。
    pub assistant_output: String,
}

/// 创建一轮助手回答所需的不可变输入和实时消息通道。
pub(crate) struct AssistantRunRequest {
    /// 本轮作为当前 prompt 的一个或多个用户消息。
    pub initial_user_messages: Vec<String>,
    /// 已完成的跨 Provider 中立对话历史。
    pub history: Vec<AssistantHistoryTurn>,
    /// 已规范化配置快照。
    pub config: AiConfig,
    /// 当前选择的模型快照。
    pub model: AiModelProfile,
    /// 全部已加载来源的不可变工作区清单。
    pub scope: Arc<SourceScopeSnapshot>,
    /// 从系统凭据库读取的密钥。
    pub api_key: SecretString,
    /// 只终止当前回答的取消令牌。
    pub cancellation: tokio_util::sync::CancellationToken,
    /// 当前回答期间追加提示的接收端。
    pub user_message_receiver: async_channel::Receiver<crate::agent::session::AgentUserMessage>,
    /// 发往助手面板的流式事件。
    pub event_sender: async_channel::Sender<crate::agent::session::AgentEvent>,
    /// 与界面共享的未消费追加提示数量。
    pub pending_user_messages: Arc<AtomicUsize>,
    /// 界面 bash 审批答复接收端；发送端由面板持有。
    pub bash_decision_receiver:
        async_channel::Receiver<crate::agent::session::BashApprovalDecision>,
}

/// 执行一轮可持续对话；完成、失败和取消终态由通用循环统一发布，不结束面板内存会话。
pub(crate) async fn run_assistant_turn(request: AssistantRunRequest) {
    let AssistantRunRequest {
        initial_user_messages,
        history,
        config,
        model,
        scope,
        api_key,
        cancellation,
        user_message_receiver,
        event_sender,
        pending_user_messages,
        bash_decision_receiver,
    } = request;
    let question = initial_user_messages.join("\n\n<USER_MESSAGE_BOUNDARY>\n\n");
    let (rig_history, history_was_trimmed) =
        build_neutral_history(&history, model.context_window_tokens);
    run_agent_loop(AgentLoopRequest {
        note: AgentLoopNote::Assistant,
        question,
        history: rig_history,
        history_was_trimmed,
        initial_user_messages,
        config,
        model,
        scope,
        api_key,
        cancellation,
        user_message_receiver,
        event_sender,
        pending_user_messages,
        user_message_gate: None,
        bash_decision_receiver,
    })
    .await;
}

/// 构造 Provider 中立的历史，并以模型配置上下文的一半作为完整问答轮次预算。
pub(crate) fn build_neutral_history(
    turns: &[AssistantHistoryTurn],
    context_window_tokens: u64,
) -> (Vec<Message>, bool) {
    let budget = (context_window_tokens / 2).max(1) as usize;
    let mut selected = Vec::new();
    let mut used = 0usize;
    for turn in turns.iter().rev() {
        let turn_tokens = turn
            .user_messages
            .iter()
            .map(|message| estimate_tokens(message))
            .sum::<usize>()
            .saturating_add(estimate_tokens(&turn.assistant_output));
        if used.saturating_add(turn_tokens) > budget {
            break;
        }
        used = used.saturating_add(turn_tokens);
        selected.push(turn);
    }
    selected.reverse();
    let was_trimmed = selected.len() < turns.len();
    let mut messages = Vec::new();
    for turn in selected {
        messages.extend(turn.user_messages.iter().cloned().map(Message::user));
        messages.push(Message::assistant(turn.assistant_output.clone()));
    }
    (messages, was_trimmed)
}

/// 对中文和 ASCII 使用保守的无 Provider 分词估算，避免引入模型专属 tokenizer。
fn estimate_tokens(text: &str) -> usize {
    let mut ascii = 0usize;
    let mut non_ascii = 0usize;
    for character in text.chars() {
        if character.is_ascii() {
            ascii += 1;
        } else {
            non_ascii += 1;
        }
    }
    ascii.saturating_add(3) / 4 + non_ascii
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证中立历史只保留可见问答，容量不足时只移除最早完整轮次。
    #[test]
    fn neutral_history_keeps_visible_turns_and_trims_oldest() {
        let turns = vec![
            AssistantHistoryTurn {
                user_messages: vec!["最早的问题".to_string()],
                assistant_output: "最早的回答".to_string(),
            },
            AssistantHistoryTurn {
                user_messages: vec!["最近的问题".to_string()],
                assistant_output: "最近的回答".to_string(),
            },
        ];

        let (history, trimmed) = build_neutral_history(&turns, 32);
        assert!(trimmed);
        let serialized = format!("{history:?}");
        assert!(!serialized.contains("最早的问题"));
        assert!(serialized.contains("最近的问题"));
    }
}
