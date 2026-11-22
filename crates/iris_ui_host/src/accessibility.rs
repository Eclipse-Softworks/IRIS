use accesskit::{Node, NodeId, Rect, Role, Tree, TreeUpdate};

use crate::protocol::A11yCommand;

#[derive(Debug, Default)]
pub struct A11yTreeState {
    commands: Vec<A11yCommand>,
}

impl A11yTreeState {
    pub fn begin_frame(&mut self) {
        self.commands.clear();
    }

    pub fn push_node(&mut self, cmd: &A11yCommand) {
        self.commands.push(cmd.clone());
    }

    pub fn node_count(&self) -> usize {
        self.commands.len()
    }

    pub fn commands(&self) -> &[A11yCommand] {
        &self.commands
    }

    /// Cheap order-sensitive fingerprint of the whole command list, so the
    /// host can skip rebuilding and re-emitting an identical AccessKit tree
    /// every frame just because a frame was presented.
    pub fn fingerprint(&self) -> u64 {
        const OFFSET: u64 = 0xcbf29ce484222325;
        const PRIME: u64 = 0x00000100000001b3;
        fn mix(mut hash: u64, bytes: &[u8]) -> u64 {
            for &b in bytes {
                hash ^= u64::from(b);
                hash = hash.wrapping_mul(PRIME);
            }
            hash
        }
        let mut h = OFFSET;
        h = mix(h, &(self.commands.len() as u64).to_le_bytes());
        for cmd in &self.commands {
            for v in [cmd.node_id, cmd.parent_id, cmd.role as u64] {
                h = mix(h, &v.to_le_bytes());
            }
            for v in [cmd.x, cmd.y, cmd.w, cmd.h] {
                h = mix(h, &v.to_bits().to_le_bytes());
            }
            h = mix(h, &(cmd.state_flags as u64).to_le_bytes());
            h = mix(h, cmd.label.as_bytes());
        }
        h
    }

    pub fn build_tree_update(&self, title: &str) -> TreeUpdate {
        let root_id = NodeId(0);
        let mut root_node = Node::new(Role::Window);
        root_node.set_label(title.to_owned());

        let mut root_children = Vec::new();
        let mut children_map: std::collections::BTreeMap<u64, Vec<NodeId>> =
            std::collections::BTreeMap::new();
        for cmd in &self.commands {
            let nid = NodeId(cmd.node_id.max(1));
            if cmd.parent_id == 0 {
                root_children.push(nid);
            } else {
                children_map.entry(cmd.parent_id).or_default().push(nid);
            }
        }
        root_node.set_children(root_children);

        let mut focused: Option<NodeId> = None;
        let mut nodes: Vec<(NodeId, Node)> = Vec::with_capacity(self.commands.len() + 1);
        nodes.push((root_id, root_node));

        for cmd in &self.commands {
            let raw_id = cmd.node_id.max(1);
            let nid = NodeId(raw_id);
            let role = map_role(cmd.role);
            let mut b = Node::new(role);
            if !cmd.label.is_empty() {
                if role == Role::Label {
                    b.set_value(cmd.label.clone());
                } else {
                    b.set_label(cmd.label.clone());
                }
            }
            b.set_bounds(Rect::new(
                cmd.x as f64,
                cmd.y as f64,
                (cmd.x + cmd.w) as f64,
                (cmd.y + cmd.h) as f64,
            ));
            if (cmd.state_flags & 1) != 0 {
                b.set_disabled();
            }
            if (cmd.state_flags & 2) != 0 {
                b.set_toggled(accesskit::Toggled::True);
            }
            if (cmd.state_flags & 4) != 0 {
                focused = Some(nid);
            }
            if let Some(ch) = children_map.remove(&raw_id) {
                b.set_children(ch);
            }
            nodes.push((nid, b));
        }

        TreeUpdate {
            nodes,
            tree: Some(Tree::new(root_id)),
            focus: focused.unwrap_or(root_id),
        }
    }
}

fn map_role(role: i32) -> Role {
    match role {
        1 => Role::Window,
        2 => Role::Label,
        3 => Role::Button,
        4 => Role::TextInput,
        5 => Role::CheckBox,
        6 => Role::Switch,
        7 => Role::Slider,
        8 => Role::List,
        9 => Role::ListItem,
        10 => Role::Canvas,
        _ => Role::GenericContainer,
    }
}
