use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    pub path: PathBuf,
    pub size: u64,
    pub children: Vec<Node>,
}

impl Node {
    pub fn new(name: String, path: PathBuf, size: u64) -> Self {
        Self {
            name,
            path,
            size,
            children: Vec::new(),
        }
    }

    pub fn insert_relative(&mut self, relative_path: &Path, leaf_size: u64) {
        let components: Vec<Component<'_>> = relative_path.components().collect();
        if components.is_empty() {
            return;
        }

        self.insert_components(&components, 0, leaf_size);
    }

    fn insert_components(&mut self, components: &[Component<'_>], index: usize, leaf_size: u64) {
        if index >= components.len() {
            return;
        }

        let component = components[index];
        let component_name = component.as_os_str().to_string_lossy().to_string();

        if component_name.is_empty() || component_name == "." {
            self.insert_components(components, index + 1, leaf_size);
            return;
        }

        let child_index = match self
            .children
            .iter()
            .position(|child| child.name == component_name)
        {
            Some(index) => index,
            None => {
                let child_path = self.path.join(&component_name);
                self.children
                    .push(Node::new(component_name.clone(), child_path, 0));
                self.children.len() - 1
            }
        };

        let is_leaf = index + 1 == components.len();
        let child = &mut self.children[child_index];

        if is_leaf {
            child.size = leaf_size;
            return;
        }

        child.insert_components(components, index + 1, leaf_size);
    }

    pub fn compute_total_size(&mut self) -> u64 {
        if self.children.is_empty() {
            return self.size;
        }

        let mut total = 0_u64;
        for child in &mut self.children {
            total = total.saturating_add(child.compute_total_size());
        }

        self.size = total;
        total
    }

    pub fn sort_children_by_size_desc(&mut self) {
        self.children.sort_by(|a, b| b.size.cmp(&a.size));
        for child in &mut self.children {
            child.sort_children_by_size_desc();
        }
    }
}
