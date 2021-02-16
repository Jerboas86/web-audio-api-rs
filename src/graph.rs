use std::collections::{HashMap, HashSet};
use std::fmt::Debug;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Receiver;

use crate::buffer::ChannelConfig;
use crate::control::ControlMessage;
use crate::{buffer::AudioBuffer, buffer::ChannelCountMode};

/// Operations running off the system-level audio callback
pub(crate) struct RenderThread {
    graph: Graph,
    sample_rate: u32,
    channels: usize,
    frames_played: AtomicU64,
    receiver: Receiver<ControlMessage>,
}

impl RenderThread {
    pub fn new<N: Render + 'static>(
        root: N,
        sample_rate: u32,
        channel_config: ChannelConfig,
        receiver: Receiver<ControlMessage>,
    ) -> Self {
        let channels = channel_config.count();

        Self {
            graph: Graph::new(root, channel_config),
            sample_rate,
            channels,
            frames_played: AtomicU64::new(0),
            receiver,
        }
    }

    fn handle_control_messages(&mut self) {
        for msg in self.receiver.try_iter() {
            use ControlMessage::*;

            match msg {
                RegisterNode {
                    id,
                    node,
                    inputs,
                    outputs,
                    channel_config,
                } => {
                    self.graph
                        .add_node(NodeIndex(id), node, inputs, outputs, channel_config);
                }
                ConnectNode {
                    from,
                    to,
                    output,
                    input,
                } => {
                    self.graph
                        .add_edge((NodeIndex(from), output), (NodeIndex(to), input));
                }
                DisconnectNode { from, to } => {
                    self.graph.remove_edge(NodeIndex(from), NodeIndex(to));
                }
                DisconnectAll { from } => {
                    self.graph.remove_edges_from(NodeIndex(from));
                }
            }
        }
    }

    pub fn render(&mut self, data: &mut [f32]) {
        // handle addition/removal of nodes/edges
        self.handle_control_messages();

        // update time
        let len = data.len() / self.channels as usize;
        let timestamp = self.frames_played.fetch_add(len as u64, Ordering::SeqCst) as f64
            / self.sample_rate as f64;

        // render audio graph
        let rendered = self.graph.render(timestamp, self.sample_rate);

        // copy rendered audio into output slice
        for i in 0..self.channels {
            let output = data.iter_mut().skip(i).step_by(self.channels);
            if let Some(channel_data) = rendered.channel_data(i) {
                let channel = channel_data.iter();
                for (sample, input) in output.zip(channel) {
                    let value = cpal::Sample::from::<f32>(input);
                    *sample = value;
                }
            } else {
                for o in output {
                    *o = cpal::Sample::from::<f32>(&0.);
                }
            }
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct NodeIndex(u64);

struct Node {
    processor: Box<dyn Render>,
    buffers: Vec<AudioBuffer>,
    inputs: usize,
    outputs: usize,
    channel_config: ChannelConfig,
}

impl Node {
    fn process(&mut self, inputs: &[&AudioBuffer], timestamp: f64, sample_rate: u32) {
        self.processor
            .process(inputs, &mut self.buffers[..], timestamp, sample_rate)
    }
}

pub(crate) struct Graph {
    nodes: HashMap<NodeIndex, Node>,

    // connections, from (node,output) to (node,input)
    edges: HashSet<((NodeIndex, u32), (NodeIndex, u32))>,

    marked: Vec<NodeIndex>,
    ordered: Vec<NodeIndex>,
}

pub trait Render: Send {
    fn process(
        &mut self,
        inputs: &[&AudioBuffer],
        outputs: &mut [AudioBuffer],
        timestamp: f64,
        sample_rate: u32,
    );
}

impl Graph {
    pub fn new<N: Render + 'static>(root: N, channel_config: ChannelConfig) -> Self {
        let root_index = NodeIndex(0);

        let mut graph = Graph {
            nodes: HashMap::new(),
            edges: HashSet::new(),
            ordered: vec![root_index],
            marked: vec![root_index],
        };

        // assume root node always has 1 input and 1 output (todo)
        let inputs = 1;
        let outputs = 1;

        graph.add_node(root_index, Box::new(root), inputs, outputs, channel_config);

        graph
    }

    pub fn add_node(
        &mut self,
        index: NodeIndex,
        processor: Box<dyn Render>,
        inputs: usize,
        outputs: usize,
        channel_config: ChannelConfig,
    ) {
        let channels = 1; //todo
        self.nodes.insert(
            index,
            Node {
                processor,
                buffers: vec![AudioBuffer::new(channels, crate::BUFFER_SIZE as usize, 0); outputs],
                inputs,
                outputs,
                channel_config,
            },
        );
    }

    pub fn add_edge(&mut self, source: (NodeIndex, u32), dest: (NodeIndex, u32)) {
        self.edges.insert((source, dest));

        self.order_nodes();
    }

    pub fn remove_edge(&mut self, source: NodeIndex, dest: NodeIndex) {
        self.edges.retain(|&(s, d)| s.0 != source || d.0 != dest);

        self.order_nodes();
    }

    pub fn remove_edges_from(&mut self, source: NodeIndex) {
        self.edges.retain(|&(s, _d)| s.0 != source);

        self.order_nodes();
    }

    pub fn children(&self, node: NodeIndex) -> impl Iterator<Item = NodeIndex> + '_ {
        self.edges
            .iter()
            .filter(move |&(_s, d)| d.0 == node)
            .map(|&(s, _d)| s.0)
    }

    fn visit(&self, n: NodeIndex, marked: &mut Vec<NodeIndex>, ordered: &mut Vec<NodeIndex>) {
        if marked.contains(&n) {
            return;
        }
        marked.push(n);
        self.children(n)
            .for_each(|c| self.visit(c, marked, ordered));
        ordered.insert(0, n);
    }

    fn order_nodes(&mut self) {
        // empty ordered_nodes, and temporarily move out of self (no allocs)
        let mut ordered = std::mem::replace(&mut self.ordered, vec![]);
        ordered.resize(self.nodes.len(), NodeIndex(0));
        ordered.clear();

        // empty marked_nodes, and temporarily move out of self (no allocs)
        let mut marked = std::mem::replace(&mut self.marked, vec![]);
        marked.resize(self.nodes.len(), NodeIndex(0));
        marked.clear();

        // start by visiting the root node
        let start = NodeIndex(0);
        self.visit(start, &mut marked, &mut ordered);

        ordered.reverse();

        // re-instate vecs to prevent new allocs
        self.ordered = ordered;
        self.marked = marked;
    }

    pub fn render(&mut self, timestamp: f64, sample_rate: u32) -> &AudioBuffer {
        // split (mut) borrows
        let ordered = &self.ordered;
        let edges = &self.edges;
        let nodes = &mut self.nodes;

        ordered.iter().for_each(|index| {
            // remove node from map, re-insert later (for borrowck reasons)
            let mut node = nodes.remove(index).unwrap();
            // initial channel count
            let channels = node.channel_config.count();
            // mix all inputs together
            let mut input_bufs =
                vec![AudioBuffer::new(channels, crate::BUFFER_SIZE as usize, sample_rate); node.inputs];
            edges
                .iter()
                .filter_map(
                    move |(s, d)| {
                        if d.0 == *index {
                            Some((s, d.1))
                        } else {
                            None
                        }
                    },
                )
                .for_each(|(&(node_index, output), input)| {
                    let node = nodes.get(&node_index).unwrap();
                    let signal = &node.buffers[output as usize];

                    input_bufs[input as usize] = input_bufs[input as usize]
                        .add(signal, node.channel_config.interpretation());
                });

            let input_bufs: Vec<_> = input_bufs
                .iter_mut()
                .map(|input_buf| {
                    // up/down-mix to the desired channel count
                    let cur_channels = input_buf.number_of_channels();
                    let new_channels = match node.channel_config.count_mode() {
                        ChannelCountMode::Max => cur_channels,
                        ChannelCountMode::Explicit => node.channel_config.count(),
                        ChannelCountMode::ClampedMax => {
                            cur_channels.min(node.channel_config.count())
                        }
                    };
                    input_buf.mix(new_channels, node.channel_config.interpretation());

                    // return immutable refs
                    &*input_buf
                })
                .collect();

            node.process(&input_bufs[..], timestamp, sample_rate);

            // re-insert node in graph
            nodes.insert(*index, node);
        });

        // return buffer of destination node
        // assume only 1 output (todo)
        &nodes.get(&NodeIndex(0)).unwrap().buffers[0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone)]
    struct TestNode {}

    impl Render for TestNode {
        fn process(&mut self, _: &[&AudioBuffer], _: &mut [AudioBuffer], _: f64, _: u32) {}
    }

    #[test]
    fn test_add_remove() {
        let config: ChannelConfig = crate::buffer::ChannelConfigOptions {
            count: 2,
            mode: crate::buffer::ChannelCountMode::Explicit,
            interpretation: crate::buffer::ChannelInterpretation::Speakers,
        }
        .into();
        let mut graph = Graph::new(TestNode {}, config.clone());

        let node = Box::new(TestNode {});
        graph.add_node(NodeIndex(1), node.clone(), 1, 1, config.clone());
        graph.add_node(NodeIndex(2), node.clone(), 1, 1, config.clone());
        graph.add_node(NodeIndex(3), node.clone(), 1, 1, config.clone());

        graph.add_edge((NodeIndex(1), 0), (NodeIndex(0), 0));
        graph.add_edge((NodeIndex(2), 0), (NodeIndex(1), 0));
        graph.add_edge((NodeIndex(3), 0), (NodeIndex(0), 0));

        // sorting is not deterministic, can be either of these two
        if graph.ordered != &[NodeIndex(3), NodeIndex(2), NodeIndex(1), NodeIndex(0)] {
            assert_eq!(
                graph.ordered,
                vec![NodeIndex(2), NodeIndex(1), NodeIndex(3), NodeIndex(0)]
            );
        }

        graph.remove_edge(NodeIndex(1), NodeIndex(0));
        assert_eq!(graph.ordered, vec![NodeIndex(3), NodeIndex(0)]);
    }

    #[test]
    fn test_remove_all() {
        let config: ChannelConfig = crate::buffer::ChannelConfigOptions {
            count: 2,
            mode: crate::buffer::ChannelCountMode::Explicit,
            interpretation: crate::buffer::ChannelInterpretation::Speakers,
        }
        .into();
        let mut graph = Graph::new(TestNode {}, config.clone());

        let node = Box::new(TestNode {});
        graph.add_node(NodeIndex(1), node.clone(), 1, 1, config.clone());
        graph.add_node(NodeIndex(2), node.clone(), 1, 1, config.clone());

        graph.add_edge((NodeIndex(1), 0), (NodeIndex(0), 0));
        graph.add_edge((NodeIndex(2), 0), (NodeIndex(0), 0));
        graph.add_edge((NodeIndex(2), 0), (NodeIndex(1), 0));

        assert_eq!(
            graph.ordered,
            vec![NodeIndex(2), NodeIndex(1), NodeIndex(0)]
        );

        graph.remove_edges_from(NodeIndex(2));

        assert_eq!(graph.ordered, vec![NodeIndex(1), NodeIndex(0)]);
    }
}
