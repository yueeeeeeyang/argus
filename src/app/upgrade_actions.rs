//! 文件职责：提取升级检查、下载、安装和跳过等升级相关方法到独立子模块。

use super::*;

impl ArgusApp {
    /// 启动升级检查任务。
    ///
    /// 参数说明：
    /// - `is_manual`：是否由用户在设置页手动触发；手动检查会忽略“已跳过版本”并显示失败提示。
    /// - `cx`：应用上下文，用于调度后台网络任务并在完成后刷新 UI。
    pub fn start_upgrade_check(&mut self, is_manual: bool, cx: &mut Context<Self>) {
        if self.is_upgrade_checking {
            self.upgrade_message = Some("升级检查正在进行".to_string());
            self.placeholder_notice = "升级检查正在进行".to_string();
            return;
        }
        if !is_manual
            && (!self.config.upgrade.enabled
                || self.config.upgrade.server_url.is_empty()
                || self.config.upgrade.public_key_base64.is_empty())
        {
            return;
        }
        if is_manual && self.config.upgrade.server_url.is_empty() {
            self.upgrade_message = Some("请先配置升级服务器地址".to_string());
            self.upgrade_dialog = Some(UpgradeDialogState::Failed {
                version: None,
                message: "请先配置升级服务器地址".to_string(),
            });
            self.placeholder_notice = "请先配置升级服务器地址".to_string();
            return;
        }
        if is_manual && self.config.upgrade.public_key_base64.is_empty() {
            self.upgrade_message = Some("请先配置升级验签公钥".to_string());
            self.upgrade_dialog = Some(UpgradeDialogState::Failed {
                version: None,
                message: "请先配置升级验签公钥".to_string(),
            });
            self.placeholder_notice = "请先配置升级验签公钥".to_string();
            return;
        }

        self.is_upgrade_checking = true;
        self.upgrade_message = Some("正在检查新版本...".to_string());
        self.placeholder_notice = if is_manual {
            "正在手动检查新版本".to_string()
        } else {
            "正在后台检查新版本".to_string()
        };
        let mut upgrade_config = self.config.upgrade.clone();
        if is_manual {
            upgrade_config.enabled = true;
        }

        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    UpgradeService::runtime(&upgrade_config).check_for_update(
                        &upgrade_config,
                        env!("CARGO_PKG_VERSION"),
                        !is_manual,
                    )
                })
                .await;

            view.update(cx, |app, cx| {
                app.is_upgrade_checking = false;
                app.config.upgrade.last_check_at = Some(chrono::Utc::now().to_rfc3339());
                match result {
                    Ok(UpgradeCheckOutcome::Disabled) => {
                        app.upgrade_message = Some("自动升级未启用或未配置服务器".to_string());
                        if is_manual {
                            app.upgrade_dialog = Some(UpgradeDialogState::Failed {
                                version: None,
                                message: "自动升级未启用或未配置服务器".to_string(),
                            });
                        }
                    }
                    Ok(UpgradeCheckOutcome::UpToDate) => {
                        app.upgrade_message = Some("当前已是最新版本".to_string());
                        if is_manual {
                            app.placeholder_notice = "当前已是最新版本".to_string();
                        }
                    }
                    Ok(UpgradeCheckOutcome::Skipped(version)) => {
                        app.upgrade_message = Some(format!("已跳过版本 {version}"));
                    }
                    Ok(UpgradeCheckOutcome::Available(upgrade)) => {
                        let version = upgrade.version.clone();
                        app.upgrade_message = Some(format!("发现新版本 {version}"));
                        app.placeholder_notice = format!("发现新版本 {version}");
                        app.upgrade_dialog = Some(UpgradeDialogState::Available { upgrade });
                    }
                    Err(error) => {
                        let message = error.to_string();
                        app.upgrade_message = Some(message.clone());
                        app.placeholder_notice = format!("升级检查失败：{message}");
                        if is_manual {
                            app.upgrade_dialog = Some(UpgradeDialogState::Failed {
                                version: None,
                                message,
                            });
                        }
                    }
                }
                app.persist_config_or_report();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 关闭升级弹窗，保留已经记录的升级消息。
    pub fn dismiss_upgrade_dialog(&mut self) {
        self.upgrade_dialog = None;
        self.placeholder_notice = "已关闭升级提示".to_string();
    }

    /// 跳过当前弹窗中的升级版本，并持久化到配置。
    pub fn skip_available_upgrade(&mut self) {
        let Some(UpgradeDialogState::Available { upgrade }) = self.upgrade_dialog.clone() else {
            return;
        };
        self.config.upgrade.skipped_version = Some(upgrade.version.clone());
        self.upgrade_message = Some(format!("已跳过版本 {}", upgrade.version));
        self.placeholder_notice = format!("已跳过版本 {}", upgrade.version);
        self.upgrade_dialog = None;
        self.persist_config_or_report();
    }

    /// 下载、校验并安装当前弹窗中的升级版本，成功后自动重启 Argus。
    pub fn install_available_upgrade(&mut self, cx: &mut Context<Self>) {
        if self.is_upgrade_installing {
            self.upgrade_message = Some("升级安装正在进行".to_string());
            return;
        }
        let Some(UpgradeDialogState::Available { upgrade }) = self.upgrade_dialog.clone() else {
            self.upgrade_dialog = Some(UpgradeDialogState::Failed {
                version: None,
                message: "没有可安装的新版本".to_string(),
            });
            return;
        };

        let version = upgrade.version.clone();
        self.is_upgrade_installing = true;
        self.upgrade_message = Some(format!("正在下载版本 {version}..."));
        self.placeholder_notice = format!("正在下载版本 {version}");
        self.upgrade_dialog = Some(UpgradeDialogState::Progress {
            version: version.clone(),
            message: "正在下载并校验升级包...".to_string(),
        });
        let upgrade_config = self.config.upgrade.clone();

        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let service = UpgradeService::runtime(&upgrade_config);
                    let prepared = service.download_and_prepare(&upgrade)?;
                    service.install_prepared_upgrade(&prepared)
                })
                .await;

            view.update(cx, |app, cx| {
                app.is_upgrade_installing = false;
                match result {
                    Ok(()) => {
                        app.upgrade_message = Some(format!("版本 {version} 已安装，正在重启"));
                        app.placeholder_notice = format!("版本 {version} 已安装，正在重启");
                        app.upgrade_dialog = Some(UpgradeDialogState::Progress {
                            version: version.clone(),
                            message: "已启动新版本，正在退出旧进程...".to_string(),
                        });
                        cx.notify();
                        cx.quit();
                    }
                    Err(error) => {
                        let message = error.to_string();
                        app.upgrade_message = Some(message.clone());
                        app.placeholder_notice = format!("升级安装失败：{message}");
                        app.upgrade_dialog = Some(UpgradeDialogState::Failed {
                            version: Some(version),
                            message,
                        });
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// 记录用户触发了尚未实现的操作。
    /// 切换自动升级开关；仅影响启动后的自动检查，不影响设置页手动检查。
    pub fn toggle_upgrade_enabled(&mut self) {
        self.config.upgrade.enabled = !self.config.upgrade.enabled;
        self.placeholder_notice = if self.config.upgrade.enabled {
            "已启用启动时自动检查升级".to_string()
        } else {
            "已关闭启动时自动检查升级".to_string()
        };
        self.persist_config_or_report();
    }

    /// 返回当前平台在升级 manifest 中使用的展示文案。
    pub fn upgrade_platform_label(&self) -> String {
        format!("{}/{}", current_platform_os(), current_platform_arch())
    }
}
