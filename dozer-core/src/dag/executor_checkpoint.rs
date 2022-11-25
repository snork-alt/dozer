use crate::dag::dag::{Dag, Edge, NodeType};
use crate::dag::errors::ExecutionError;
use crate::dag::errors::ExecutionError::InvalidCheckpointState;
use crate::dag::executor_utils::CHECKPOINT_DB_NAME;
use crate::dag::forwarder::{
    INPUT_SCHEMA_IDENTIFIER, OUTPUT_SCHEMA_IDENTIFIER, SOURCE_ID_IDENTIFIER,
};
use crate::dag::node::{NodeHandle, PortHandle};

use crate::storage::errors::StorageError;
use crate::storage::errors::StorageError::DeserializationError;
use crate::storage::lmdb_storage::LmdbEnvironmentManager;
use dozer_types::types::Schema;
use std::collections::HashMap;
use std::path::Path;

pub(crate) enum Consistency {
    FullyConsistent(u64),
    PartiallyConsistent(HashMap<u64, Vec<NodeHandle>>),
}

struct DependencyTreeNode {
    pub handle: NodeHandle,
    pub children: Vec<DependencyTreeNode>,
}

impl DependencyTreeNode {
    pub fn new(handle: NodeHandle) -> Self {
        Self {
            handle,
            children: Vec::new(),
        }
    }
}

pub(crate) struct CheckpointMetadata {
    pub commits: HashMap<NodeHandle, u64>,
    pub input_schemas: HashMap<PortHandle, Schema>,
    pub output_schemas: HashMap<PortHandle, Schema>,
}

pub(crate) struct CheckpointMetadataReader<'a> {
    dag: &'a Dag,
    path: &'a Path,
    metadata: HashMap<NodeHandle, CheckpointMetadata>,
    deps_trees: HashMap<NodeHandle, DependencyTreeNode>,
}

impl<'a> CheckpointMetadataReader<'a> {
    pub fn new(
        dag: &'a Dag,
        path: &'a Path,
    ) -> Result<CheckpointMetadataReader<'a>, ExecutionError> {
        let metadata = CheckpointMetadataReader::get_checkpoint_metadata(path, dag)?;
        let mut deps_trees: HashMap<NodeHandle, DependencyTreeNode> = HashMap::new();

        for src in dag
            .nodes
            .iter()
            .filter(|e| matches!(e.1, NodeType::Source(_)))
            .map(|e| e.0)
        {
            let mut root = DependencyTreeNode::new(src.clone());
            Self::get_source_dependency_tree(&mut root, dag);
            deps_trees.insert(src.clone(), root);
        }

        Ok(Self {
            path,
            dag,
            metadata,
            deps_trees,
        })
    }

    fn get_node_checkpoint_metadata(
        path: &Path,
        name: &NodeHandle,
    ) -> Result<CheckpointMetadata, ExecutionError> {
        if !LmdbEnvironmentManager::exists(path, name) {
            return Err(InvalidCheckpointState(name.clone()));
        }

        let mut env = LmdbEnvironmentManager::create(path, name)?;
        let db = env.open_database(CHECKPOINT_DB_NAME, false)?;
        let txn = env.create_txn()?;

        let cur = txn.open_cursor(&db)?;
        if !cur.first()? {
            return Err(ExecutionError::InternalDatabaseError(
                StorageError::InvalidRecord,
            ));
        }

        let mut map = HashMap::<NodeHandle, u64>::new();
        let mut input_schemas: HashMap<PortHandle, Schema> = HashMap::new();
        let mut output_schemas: HashMap<PortHandle, Schema> = HashMap::new();

        loop {
            let value = cur.read()?.ok_or(ExecutionError::InternalDatabaseError(
                StorageError::InvalidRecord,
            ))?;
            match value.0[0] {
                SOURCE_ID_IDENTIFIER => {
                    let handle: NodeHandle = String::from_utf8_lossy(&value.0[1..]).to_string();
                    let seq: u64 = u64::from_be_bytes(value.1.try_into().unwrap());
                    map.insert(handle, seq);
                }
                OUTPUT_SCHEMA_IDENTIFIER => {
                    let handle: PortHandle = PortHandle::from_be_bytes(
                        (&value.0[1..])
                            .try_into()
                            .map_err(|_e| ExecutionError::InvalidPortHandle(0))?,
                    );
                    let schema: Schema =
                        bincode::deserialize(value.1).map_err(|e| DeserializationError {
                            typ: "Schema".to_string(),
                            reason: Box::new(e),
                        })?;
                    output_schemas.insert(handle, schema);
                }
                INPUT_SCHEMA_IDENTIFIER => {
                    let handle: PortHandle = PortHandle::from_be_bytes(
                        (&value.0[1..])
                            .try_into()
                            .map_err(|_e| ExecutionError::InvalidPortHandle(0))?,
                    );
                    let schema: Schema =
                        bincode::deserialize(value.1).map_err(|e| DeserializationError {
                            typ: "Schema".to_string(),
                            reason: Box::new(e),
                        })?;
                    input_schemas.insert(handle, schema);
                }
                _ => {
                    return Err(ExecutionError::InternalDatabaseError(
                        StorageError::InvalidRecord,
                    ))
                }
            }
            if !cur.next()? {
                break;
            }
        }

        Ok(CheckpointMetadata {
            commits: map,
            input_schemas,
            output_schemas,
        })
    }

    pub(crate) fn get_checkpoint_metadata(
        path: &Path,
        dag: &Dag,
    ) -> Result<HashMap<NodeHandle, CheckpointMetadata>, ExecutionError> {
        let mut all = HashMap::<NodeHandle, CheckpointMetadata>::new();
        for node in &dag.nodes {
            match CheckpointMetadataReader::get_node_checkpoint_metadata(path, node.0) {
                Ok(r) => {
                    all.insert(node.0.clone(), r);
                }
                Err(_e) => LmdbEnvironmentManager::remove(path, node.0),
            }
        }
        Ok(all)
    }

    fn get_state_seq_for_node(
        &self,
        src: &NodeHandle,
        curr: &NodeHandle,
    ) -> Result<u64, ExecutionError> {
        let node_meta = self
            .metadata
            .get(curr)
            .ok_or_else(|| ExecutionError::InvalidCheckpointState(curr.clone()))?;
        node_meta
            .commits
            .get(src)
            .copied()
            .ok_or_else(|| ExecutionError::InvalidCheckpointState(curr.clone()))
    }

    fn get_source_dependency_tree(curr: &mut DependencyTreeNode, dag: &Dag) {
        let children: Vec<&Edge> = dag
            .edges
            .iter()
            .filter(|e| e.from.node == curr.handle)
            .collect();

        for child in children {
            let mut new_node = DependencyTreeNode::new(child.to.node.clone());
            Self::get_source_dependency_tree(&mut new_node, dag);
            curr.children.push(new_node);
        }
    }

    fn get_dependency_tree_consistency_rec(
        &self,
        source_handle: &NodeHandle,
        tree_node: &DependencyTreeNode,
        res: &mut HashMap<u64, Vec<NodeHandle>>,
    ) {
        let seq = match self.metadata.get(&tree_node.handle) {
            Some(v) => *v.commits.get(source_handle).unwrap_or(&0),
            None => 0,
        };

        res.entry(seq).or_insert_with(Vec::new);
        res.get_mut(&seq).unwrap().push(tree_node.handle.clone());

        for child in &tree_node.children {
            self.get_dependency_tree_consistency_rec(source_handle, child, res);
        }
    }

    pub(crate) fn get_dependency_tree_consistency(&self) -> HashMap<NodeHandle, Consistency> {
        let mut r: HashMap<NodeHandle, Consistency> = HashMap::new();
        for e in &self.deps_trees {
            let mut res: HashMap<u64, Vec<NodeHandle>> = HashMap::new();
            self.get_dependency_tree_consistency_rec(&e.1.handle, e.1, &mut res);
            match res.len() {
                1 => r.insert(
                    e.0.clone(),
                    Consistency::FullyConsistent(*res.iter().next().unwrap().0),
                ),
                _ => r.insert(e.0.clone(), Consistency::PartiallyConsistent(res)),
            };
        }
        r
    }

    // fn get_state_schema_for_node(
    //     &self,
    //     node: &NodeHandle,
    // ) -> Result<HashMap<PortHandle, Schema>, ExecutionError> {
    //     let node_meta = self
    //         .metadata
    //         .get(node)
    //         .ok_or_else(|| ExecutionError::InvalidCheckpointState(node.clone()))?;
    //     Ok(node_meta.schemas.clone())
    // }

    // pub fn get_source_checkpointing_consistency(
    //     &self,
    //     source_handle: &NodeHandle,
    // ) -> Result<CheckpointConsistency, ExecutionError> {
    //     let curr_node = source_handle;
    //     let all_seqs = Vec::<u64>::new();
    //     loop {
    //         if self.dag.is_stateful(curr_node)? {
    //             let seq = self.get_state_seq_for_node(source_handle, curr_node);
    //         }
    //     }
    //     Ok(true)
    // }
}