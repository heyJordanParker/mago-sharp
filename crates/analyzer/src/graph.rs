//! Graph walks the analyzer and the checker's back ends share.

/// The strongly connected parts of a graph, each after every part it reaches.
///
/// `successors[node]` holds the nodes `node` has an edge to. This is Tarjan's algorithm without recursion, so a long
/// chain of calls cannot overflow the stack.
#[must_use]
pub fn strongly_connected_parts(successors: &[Vec<usize>]) -> Vec<Vec<usize>> {
    const UNVISITED: usize = usize::MAX;

    let mut order = vec![UNVISITED; successors.len()];
    let mut low = vec![0; successors.len()];
    let mut on_stack = vec![false; successors.len()];
    let mut stack = Vec::new();
    let mut parts = Vec::new();
    let mut next = 0;

    for root in 0..successors.len() {
        if order[root] != UNVISITED {
            continue;
        }

        let mut work = vec![(root, 0)];
        while let Some((node, mut position)) = work.pop() {
            if position == 0 {
                order[node] = next;
                low[node] = next;
                next += 1;
                stack.push(node);
                on_stack[node] = true;
            }

            let mut descended = false;
            while let Some(&successor) = successors[node].get(position) {
                position += 1;
                if order[successor] == UNVISITED {
                    work.push((node, position));
                    work.push((successor, 0));
                    descended = true;
                    break;
                }

                if on_stack[successor] {
                    low[node] = low[node].min(order[successor]);
                }
            }

            if descended {
                continue;
            }

            if low[node] == order[node] {
                let mut part = Vec::new();
                while let Some(member) = stack.pop() {
                    on_stack[member] = false;
                    part.push(member);
                    if member == node {
                        break;
                    }
                }

                parts.push(part);
            }

            if let Some(&(parent, _)) = work.last() {
                low[parent] = low[parent].min(low[node]);
            }
        }
    }

    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(parts: Vec<Vec<usize>>) -> Vec<Vec<usize>> {
        parts
            .into_iter()
            .map(|mut part| {
                part.sort_unstable();
                part
            })
            .collect()
    }

    #[test]
    fn a_cycle_is_one_part_after_the_part_it_reaches() {
        let successors = vec![vec![1], vec![0, 2], vec![]];

        assert_eq!(sorted(strongly_connected_parts(&successors)), [vec![2], vec![0, 1]]);
    }

    #[test]
    fn a_chain_gives_one_part_per_node_with_the_last_first() {
        let successors = vec![vec![1], vec![2], vec![]];

        assert_eq!(sorted(strongly_connected_parts(&successors)), [vec![2], vec![1], vec![0]]);
    }

    #[test]
    fn a_long_chain_does_not_overflow_the_stack() {
        let successors: Vec<Vec<usize>> = (0..200_000).map(|node| vec![(node + 1) % 200_000]).collect();

        assert_eq!(strongly_connected_parts(&successors).len(), 1);
    }
}
