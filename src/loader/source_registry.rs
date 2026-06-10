//! 文件职责：管理日志来源树节点注册和可见索引。
//! 创建日期：2026-06-09
//! 修改日期：2026-06-10
//! 作者：Argus 开发团队
//! 主要功能：集中维护来源 ID、父子关系、选择状态和虚拟列表可见节点。

use std::collections::HashMap;

use crate::loader::log_source::{SourceId, SourceTreeNode};

/// 来源树单行渲染元数据，随树结构变化统一重建，避免滚动时重复计算。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SourceRowMeta {
    /// 当前节点直接子级数量。
    pub child_count: usize,
    /// 当前节点后方是否还有同级节点。
    pub has_next_sibling: bool,
    /// 需要延续竖向连线的祖先层级。
    pub ancestor_continuation_levels: Vec<usize>,
}

/// 来源注册表，使用扁平存储和父子索引支撑大目录树。
#[derive(Clone, Debug)]
pub struct SourceRegistry {
    /// 下一个可分配来源 ID。
    next_id: usize,
    /// 根节点顺序。
    root_ids: Vec<SourceId>,
    /// 节点存储表。
    nodes: HashMap<SourceId, SourceTreeNode>,
    /// 父节点到子节点的顺序索引。
    children: HashMap<SourceId, Vec<SourceId>>,
    /// 当前展开状态下应渲染的扁平节点 ID 列表。
    visible_source_ids: Vec<SourceId>,
    /// 已加载节点的稳定树形顺序，不受展开状态影响。
    tree_order_source_ids: Vec<SourceId>,
    /// 来源节点搜索用小写关键字，避免每次输入时重复分配字符串。
    search_keys: HashMap<SourceId, String>,
    /// 来源树行渲染元数据缓存，供虚拟列表滚动时直接读取。
    row_meta: HashMap<SourceId, SourceRowMeta>,
}

impl Default for SourceRegistry {
    /// 构造空来源注册表。
    fn default() -> Self {
        Self {
            next_id: 1,
            root_ids: Vec::new(),
            nodes: HashMap::new(),
            children: HashMap::new(),
            visible_source_ids: Vec::new(),
            tree_order_source_ids: Vec::new(),
            search_keys: HashMap::new(),
            row_meta: HashMap::new(),
        }
    }
}

impl SourceRegistry {
    /// 创建空来源注册表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 返回是否不包含任何来源节点。
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// 返回节点总数。
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// 分配新的来源 ID。
    pub fn allocate_id(&mut self) -> SourceId {
        let id = SourceId(self.next_id);
        self.next_id += 1;
        id
    }

    /// 注册一个节点，并维护根节点或父子关系顺序。
    pub fn insert_node(&mut self, node: SourceTreeNode) {
        let id = node.id;
        self.search_keys.insert(id, search_key_for_node(&node));
        if let Some(parent_id) = node.parent_id {
            self.children.entry(parent_id).or_default().push(id);
        } else {
            self.root_ids.push(id);
        }
        self.nodes.insert(id, node);
    }

    /// 按 ID 读取节点。
    pub fn node(&self, id: SourceId) -> Option<&SourceTreeNode> {
        self.nodes.get(&id)
    }

    /// 按 ID 可变读取节点。
    pub fn node_mut(&mut self, id: SourceId) -> Option<&mut SourceTreeNode> {
        self.nodes.get_mut(&id)
    }

    /// 返回父节点的直接子节点 ID。
    pub fn child_ids(&self, parent_id: SourceId) -> &[SourceId] {
        self.children
            .get(&parent_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// 返回指定节点后方是否还有同级节点，用于目录树连线避免绘制多余竖线。
    pub fn has_next_sibling(&self, id: SourceId) -> bool {
        self.row_meta
            .get(&id)
            .map(|meta| meta.has_next_sibling)
            .unwrap_or(false)
    }

    /// 返回需要延续竖向连线的祖先层级，供虚拟列表行按需绘制树形上下文。
    pub fn ancestor_continuation_levels(&self, id: SourceId) -> &[usize] {
        self.row_meta
            .get(&id)
            .map(|meta| meta.ancestor_continuation_levels.as_slice())
            .unwrap_or(&[])
    }

    /// 返回指定节点的行渲染元数据。
    pub fn row_meta(&self, id: SourceId) -> SourceRowMeta {
        self.row_meta.get(&id).cloned().unwrap_or_default()
    }

    /// 返回当前可见节点 ID 列表。
    pub fn visible_source_ids(&self) -> &[SourceId] {
        &self.visible_source_ids
    }

    /// 返回所有已加载节点的树形顺序 ID 列表，不受展开状态影响。
    pub fn tree_order_source_ids(&self) -> &[SourceId] {
        &self.tree_order_source_ids
    }

    /// 返回指定节点的预计算搜索关键字。
    pub fn search_key(&self, id: SourceId) -> Option<&str> {
        self.search_keys.get(&id).map(String::as_str)
    }

    /// 返回指定节点从根到父级的祖先链路，供过滤视图保留目录上下文。
    pub fn ancestor_ids(&self, id: SourceId) -> Vec<SourceId> {
        let mut ancestors = Vec::new();
        let mut current_parent_id = self.node(id).and_then(|node| node.parent_id);

        while let Some(parent_id) = current_parent_id {
            let Some(parent) = self.node(parent_id) else {
                break;
            };
            ancestors.push(parent_id);
            current_parent_id = parent.parent_id;
        }

        ancestors.reverse();
        ancestors
    }

    /// 选择指定节点，并取消其他节点选中态。
    pub fn select(&mut self, selected_id: SourceId) -> Option<SourceTreeNode> {
        let mut selected = None;

        for node in self.nodes.values_mut() {
            node.selected = node.id == selected_id;
            if node.selected {
                selected = Some(node.clone());
            }
        }

        selected
    }

    /// 切换节点展开状态，返回切换后的状态。
    pub fn toggle_expanded(&mut self, id: SourceId) -> Option<bool> {
        let node = self.nodes.get_mut(&id)?;
        if !node.kind.can_expand() {
            return None;
        }
        node.expanded = !node.expanded;
        let expanded = node.expanded;
        self.rebuild_visible_index();
        Some(expanded)
    }

    /// 收起全部可展开节点并重建可见索引。
    pub fn collapse_all(&mut self) -> usize {
        let mut collapsed_count = 0;
        for node in self.nodes.values_mut() {
            if node.kind.can_expand() && node.expanded {
                node.expanded = false;
                collapsed_count += 1;
            }
        }
        self.rebuild_visible_index();
        collapsed_count
    }

    /// 标记节点加载状态。
    pub fn set_loading(&mut self, id: SourceId, is_loading: bool) {
        if let Some(node) = self.nodes.get_mut(&id) {
            node.metadata.is_loading = is_loading;
        }
    }

    /// 将子级注册表追加到指定父节点下，并重映射子节点 ID。
    pub fn append_children_registry(
        &mut self,
        parent_id: SourceId,
        other: SourceRegistry,
        should_expand_parent: bool,
    ) -> usize {
        self.remove_existing_children(parent_id);
        self.children.insert(parent_id, Vec::new());
        let mut id_map = HashMap::new();
        let mut added_count = 0;

        for old_id in other.root_ids.iter().copied() {
            self.append_subtree_from(
                &other,
                old_id,
                Some(parent_id),
                &mut id_map,
                &mut added_count,
            );
        }

        if let Some(parent) = self.nodes.get_mut(&parent_id) {
            parent.metadata.children_loaded = true;
            parent.metadata.is_loading = false;
            parent.metadata.message = None;
            parent.expanded = should_expand_parent;
        }
        self.rebuild_all_indices();
        added_count
    }

    /// 标记子级加载失败，保留未加载状态以便用户下次点击重试。
    pub fn mark_children_load_failed(&mut self, parent_id: SourceId, message: String) {
        if let Some(parent) = self.nodes.get_mut(&parent_id) {
            parent.metadata.children_loaded = false;
            parent.metadata.is_loading = false;
            parent.metadata.message = Some(message);
            parent.expanded = false;
        }
        self.rebuild_visible_index();
    }

    /// 清空注册表并重建可见索引。
    pub fn clear(&mut self) {
        self.root_ids.clear();
        self.nodes.clear();
        self.children.clear();
        self.visible_source_ids.clear();
        self.tree_order_source_ids.clear();
        self.search_keys.clear();
        self.row_meta.clear();
        self.next_id = 1;
    }

    /// 同时重建树序、行元数据和可见索引，供结构变化后调用。
    pub fn rebuild_all_indices(&mut self) {
        self.rebuild_tree_metadata();
        self.rebuild_visible_index();
    }

    /// 重建当前展开状态下的可见节点 ID 列表。
    pub fn rebuild_visible_index(&mut self) {
        let mut visible = Vec::new();
        for root_id in self.root_ids.iter().copied() {
            self.collect_visible(root_id, &mut visible);
        }
        self.visible_source_ids = visible;
    }

    /// 重建不随展开状态变化的树序和行元数据缓存。
    fn rebuild_tree_metadata(&mut self) {
        self.tree_order_source_ids.clear();
        self.row_meta.clear();

        let root_ids = self.root_ids.clone();
        let root_count = root_ids.len();
        for (index, root_id) in root_ids.into_iter().enumerate() {
            self.collect_tree_metadata(root_id, &[], index + 1 < root_count);
        }
    }

    /// 递归收集可见节点；只操作 ID，不创建 UI 元素。
    fn collect_visible(&self, id: SourceId, visible: &mut Vec<SourceId>) {
        visible.push(id);
        let Some(node) = self.nodes.get(&id) else {
            return;
        };
        if !node.expanded {
            return;
        }

        if let Some(child_ids) = self.children.get(&id) {
            for child_id in child_ids.iter().copied() {
                self.collect_visible(child_id, visible);
            }
        }
    }

    /// 递归收集树序和行元数据；只在结构变化时执行，避免滚动热路径重复计算。
    fn collect_tree_metadata(
        &mut self,
        id: SourceId,
        ancestor_continuation_levels: &[usize],
        has_next_sibling: bool,
    ) {
        self.tree_order_source_ids.push(id);
        let child_ids = self.children.get(&id).cloned().unwrap_or_default();
        self.row_meta.insert(
            id,
            SourceRowMeta {
                child_count: child_ids.len(),
                has_next_sibling,
                ancestor_continuation_levels: ancestor_continuation_levels.to_vec(),
            },
        );

        let Some(node) = self.nodes.get(&id) else {
            return;
        };
        let mut child_ancestor_levels = ancestor_continuation_levels.to_vec();
        if node.depth > 0 && has_next_sibling {
            child_ancestor_levels.push(node.depth - 1);
        }

        let child_count = child_ids.len();
        for (index, child_id) in child_ids.into_iter().enumerate() {
            self.collect_tree_metadata(child_id, &child_ancestor_levels, index + 1 < child_count);
        }
    }

    /// 追加另一个注册表中的子树，并为当前注册表重新分配 ID。
    fn append_subtree_from(
        &mut self,
        other: &SourceRegistry,
        old_id: SourceId,
        new_parent_id: Option<SourceId>,
        id_map: &mut HashMap<SourceId, SourceId>,
        added_count: &mut usize,
    ) -> Option<SourceId> {
        let old_node = other.node(old_id)?.clone();
        let new_id = self.allocate_id();
        id_map.insert(old_id, new_id);

        let mut new_node = old_node;
        new_node.id = new_id;
        new_node.parent_id = new_parent_id;
        if let Some(parent_id) = new_parent_id
            && let Some(parent) = self.nodes.get(&parent_id)
        {
            new_node.depth = parent.depth + 1;
        }
        self.insert_node(new_node);
        *added_count += 1;

        for child_id in other.child_ids(old_id).iter().copied() {
            self.append_subtree_from(other, child_id, Some(new_id), id_map, added_count);
        }

        Some(new_id)
    }

    /// 删除指定父节点原有子树，避免重新加载时产生不可见的孤儿节点。
    fn remove_existing_children(&mut self, parent_id: SourceId) {
        let Some(child_ids) = self.children.remove(&parent_id) else {
            return;
        };

        for child_id in child_ids {
            self.remove_subtree(child_id);
        }
    }

    /// 递归移除节点及其所有子级。
    fn remove_subtree(&mut self, id: SourceId) {
        if let Some(child_ids) = self.children.remove(&id) {
            for child_id in child_ids {
                self.remove_subtree(child_id);
            }
        }
        self.nodes.remove(&id);
        self.search_keys.remove(&id);
        self.row_meta.remove(&id);
    }
}

/// 构造来源节点搜索关键字，集中小写化以降低搜索输入时的分配成本。
fn search_key_for_node(node: &SourceTreeNode) -> String {
    format!(
        "{}\n{}",
        node.label.to_lowercase(),
        node.location.display_path().to_lowercase()
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::loader::log_source::{SourceKind, SourceLocation, SourceMetadata, SourceTreeNode};
    use crate::loader::source_registry::SourceRegistry;

    /// 构造最小节点，便于验证注册表可见索引行为。
    fn test_node(
        registry: &mut SourceRegistry,
        parent_id: Option<crate::loader::SourceId>,
        depth: usize,
        label: &str,
        expanded: bool,
    ) -> crate::loader::SourceTreeNode {
        let id = registry.allocate_id();
        SourceTreeNode {
            id,
            parent_id,
            depth,
            label: label.to_string(),
            kind: if depth == 0 {
                SourceKind::Directory
            } else {
                SourceKind::LogFile
            },
            location: SourceLocation::LocalPath(PathBuf::from(label)),
            metadata: SourceMetadata {
                size: None,
                children_loaded: true,
                is_loading: false,
                message: None,
            },
            selected: false,
            expanded,
        }
    }

    /// 验证可见索引只根据展开状态维护 ID 顺序，不依赖 UI 递归渲染。
    #[test]
    fn rebuilds_visible_index_from_expansion_state() {
        let mut registry = SourceRegistry::new();
        let root = test_node(&mut registry, None, 0, "root", true);
        let root_id = root.id;
        registry.insert_node(root);
        let first_child = test_node(&mut registry, Some(root_id), 1, "a.log", false);
        let first_child_id = first_child.id;
        let second_child = test_node(&mut registry, Some(root_id), 1, "b.log", false);
        let second_child_id = second_child.id;
        registry.insert_node(first_child);
        registry.insert_node(second_child);

        registry.rebuild_all_indices();
        assert_eq!(registry.visible_source_ids().len(), 3);
        assert_eq!(
            registry.tree_order_source_ids(),
            &[root_id, first_child_id, second_child_id]
        );
        assert_eq!(registry.ancestor_ids(first_child_id), vec![root_id]);

        registry.toggle_expanded(root_id);
        assert_eq!(registry.visible_source_ids().len(), 1);

        registry.toggle_expanded(root_id);
        assert_eq!(registry.visible_source_ids().len(), 3);
    }

    /// 验证兄弟关系可用于 UI 连线裁剪，最后一个兄弟不应继续向下绘制竖线。
    #[test]
    fn reports_sibling_continuation_for_tree_connectors() {
        let mut registry = SourceRegistry::new();
        let root = test_node(&mut registry, None, 0, "root", true);
        let root_id = root.id;
        registry.insert_node(root);
        let first_child = test_node(&mut registry, Some(root_id), 1, "first", true);
        let first_child_id = first_child.id;
        let second_child = test_node(&mut registry, Some(root_id), 1, "second", false);
        let second_child_id = second_child.id;
        registry.insert_node(first_child);
        registry.insert_node(second_child);
        let nested_log = test_node(&mut registry, Some(first_child_id), 2, "nested.log", false);
        let nested_log_id = nested_log.id;
        registry.insert_node(nested_log);
        registry.rebuild_all_indices();

        assert!(registry.has_next_sibling(first_child_id));
        assert!(!registry.has_next_sibling(second_child_id));
        assert!(
            registry
                .ancestor_continuation_levels(first_child_id)
                .is_empty()
        );
        assert_eq!(registry.ancestor_continuation_levels(nested_log_id), &[0]);
    }
}
