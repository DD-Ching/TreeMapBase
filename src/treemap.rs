use crate::model::Node;

#[derive(Debug, Clone, Copy)]
pub struct LayoutRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl LayoutRect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    fn area(self) -> f32 {
        self.w.max(0.0) * self.h.max(0.0)
    }

    fn shortest_side(self) -> f32 {
        self.w.min(self.h)
    }

    fn shrink(self, padding: f32) -> Self {
        let doubled = padding * 2.0;
        Self {
            x: self.x + padding,
            y: self.y + padding,
            w: (self.w - doubled).max(0.0),
            h: (self.h - doubled).max(0.0),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TreemapCell<'a> {
    pub node: &'a Node,
    pub rect: LayoutRect,
    pub depth: usize,
}

#[derive(Debug, Clone, Copy)]
struct RowItem<'a> {
    node: &'a Node,
    area: f32,
}

pub fn squarified_treemap<'a>(
    root: &'a Node,
    bounds: LayoutRect,
    max_depth: usize,
    max_nodes: usize,
) -> Vec<TreemapCell<'a>> {
    let mut cells = Vec::with_capacity(2048);

    if bounds.w <= 0.0 || bounds.h <= 0.0 {
        return cells;
    }

    layout_recursive(root, bounds, 0, max_depth, max_nodes, &mut cells);
    cells
}

fn layout_recursive<'a>(
    node: &'a Node,
    bounds: LayoutRect,
    depth: usize,
    max_depth: usize,
    max_nodes: usize,
    out: &mut Vec<TreemapCell<'a>>,
) {
    if out.len() >= max_nodes || bounds.w <= 0.2 || bounds.h <= 0.2 {
        return;
    }

    out.push(TreemapCell {
        node,
        rect: bounds,
        depth,
    });

    if depth >= max_depth || node.children.is_empty() {
        return;
    }

    let inner_bounds = bounds.shrink(1.0);
    if inner_bounds.w <= 0.2 || inner_bounds.h <= 0.2 {
        return;
    }

    let mut children: Vec<&Node> = node
        .children
        .iter()
        .filter(|child| child.size > 0)
        .collect();
    if children.is_empty() {
        return;
    }

    children.sort_by(|a, b| b.size.cmp(&a.size));

    let total_size: u64 = children
        .iter()
        .fold(0_u64, |sum, node| sum.saturating_add(node.size));
    if total_size == 0 {
        return;
    }

    let total_area = inner_bounds.area();
    let items: Vec<RowItem<'_>> = children
        .iter()
        .map(|node| RowItem {
            node,
            area: total_area * (node.size as f32 / total_size as f32),
        })
        .collect();

    for (item, rect) in squarify_items(&items, inner_bounds) {
        layout_recursive(item.node, rect, depth + 1, max_depth, max_nodes, out);
        if out.len() >= max_nodes {
            break;
        }
    }
}

fn squarify_items<'a>(items: &[RowItem<'a>], bounds: LayoutRect) -> Vec<(RowItem<'a>, LayoutRect)> {
    let mut output = Vec::with_capacity(items.len());
    let mut remaining = bounds;
    let mut row: Vec<RowItem<'_>> = Vec::new();

    for item in items {
        let mut expanded_row = row.clone();
        expanded_row.push(*item);

        let side = remaining.shortest_side().max(1.0);
        if row.is_empty() || worst_ratio(&expanded_row, side) <= worst_ratio(&row, side) {
            row.push(*item);
            continue;
        }

        remaining = layout_row(&row, remaining, &mut output);
        row.clear();
        row.push(*item);
    }

    if !row.is_empty() {
        let _ = layout_row(&row, remaining, &mut output);
    }

    output
}

fn layout_row<'a>(
    row: &[RowItem<'a>],
    bounds: LayoutRect,
    output: &mut Vec<(RowItem<'a>, LayoutRect)>,
) -> LayoutRect {
    let row_area: f32 = row.iter().map(|item| item.area).sum();

    if bounds.w >= bounds.h {
        // Squarified treemap places items along the shortest side.
        // When width >= height, shortest side is height, so we build a vertical strip.
        let column_width = if bounds.h > 0.0 {
            row_area / bounds.h
        } else {
            0.0
        };

        let mut y = bounds.y;
        for item in row {
            let height = if column_width > 0.0 {
                item.area / column_width
            } else {
                0.0
            };

            output.push((
                *item,
                LayoutRect::new(bounds.x, y, column_width.max(0.0), height.max(0.0)),
            ));

            y += height;
        }

        LayoutRect::new(
            bounds.x + column_width,
            bounds.y,
            (bounds.w - column_width).max(0.0),
            bounds.h,
        )
    } else {
        // When width < height, shortest side is width, so we build a horizontal strip.
        let row_height = if bounds.w > 0.0 {
            row_area / bounds.w
        } else {
            0.0
        };

        let mut x = bounds.x;
        for item in row {
            let width = if row_height > 0.0 {
                item.area / row_height
            } else {
                0.0
            };

            output.push((
                *item,
                LayoutRect::new(x, bounds.y, width.max(0.0), row_height.max(0.0)),
            ));

            x += width;
        }

        LayoutRect::new(
            bounds.x,
            bounds.y + row_height,
            bounds.w,
            (bounds.h - row_height).max(0.0),
        )
    }
}

fn worst_ratio(row: &[RowItem<'_>], side: f32) -> f32 {
    if row.is_empty() || side <= 0.0 {
        return f32::INFINITY;
    }

    let mut min_area = f32::INFINITY;
    let mut max_area = 0.0_f32;
    let mut sum = 0.0_f32;

    for item in row {
        let area = item.area.max(0.0);
        min_area = min_area.min(area);
        max_area = max_area.max(area);
        sum += area;
    }

    if min_area <= 0.0 || sum <= 0.0 {
        return f32::INFINITY;
    }

    let side_sq = side * side;
    let sum_sq = sum * sum;
    let ratio_a = side_sq * max_area / sum_sq;
    let ratio_b = sum_sq / (side_sq * min_area);
    ratio_a.max(ratio_b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn build_root_with_sizes(sizes: &[u64]) -> Node {
        let mut root = Node::new("root".to_string(), PathBuf::from("root"), 0);
        for (index, size) in sizes.iter().enumerate() {
            root.children.push(Node::new(
                format!("child_{index}"),
                PathBuf::from(format!("root/child_{index}")),
                *size,
            ));
        }
        root.compute_total_size();
        root.sort_children_by_size_desc();
        root
    }

    #[test]
    fn wide_canvas_splits_across_x_axis() {
        let root = build_root_with_sizes(&[500, 250, 125, 64, 32, 16, 8, 4]);
        let bounds = LayoutRect::new(0.0, 0.0, 1200.0, 600.0);
        let cells = squarified_treemap(&root, bounds, 1, 1024);

        let depth1_cells: Vec<_> = cells.into_iter().filter(|cell| cell.depth == 1).collect();
        assert!(
            depth1_cells.len() >= 4,
            "expected child cells to be laid out"
        );

        let mut distinct_x = Vec::<i32>::new();
        for cell in depth1_cells {
            let x = (cell.rect.x * 10.0).round() as i32;
            if !distinct_x.contains(&x) {
                distinct_x.push(x);
            }
        }

        assert!(
            distinct_x.len() > 1,
            "layout should split along x-axis on a wide canvas"
        );
    }
}
