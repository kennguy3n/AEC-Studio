//! Project + user block libraries.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::blocks::block::Block;
use crate::error::{CadError, CadResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockScope {
    Project,
    User,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BlockLibrary {
    pub project_blocks: BTreeMap<String, Block>,
    pub user_blocks: BTreeMap<String, Block>,
}

impl BlockLibrary {
    pub fn insert(&mut self, scope: BlockScope, block: Block) -> CadResult<()> {
        if block.name.is_empty() {
            return Err(CadError::InvalidLayerName("block name empty".into()));
        }
        match scope {
            BlockScope::Project => self.project_blocks.insert(block.name.clone(), block),
            BlockScope::User => self.user_blocks.insert(block.name.clone(), block),
        };
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&Block> {
        self.project_blocks
            .get(name)
            .or_else(|| self.user_blocks.get(name))
    }

    pub fn project_iter(&self) -> impl Iterator<Item = &Block> {
        self.project_blocks.values()
    }

    pub fn user_iter(&self) -> impl Iterator<Item = &Block> {
        self.user_blocks.values()
    }

    pub fn remove(&mut self, scope: BlockScope, name: &str) -> CadResult<()> {
        let map = match scope {
            BlockScope::Project => &mut self.project_blocks,
            BlockScope::User => &mut self.user_blocks,
        };
        map.remove(name)
            .map(|_| ())
            .ok_or_else(|| CadError::InvalidLayerName(name.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_block_shadows_user_block() {
        let mut lib = BlockLibrary::default();
        let mut user = Block::new("WIN");
        user.description = Some("user".into());
        let mut proj = Block::new("WIN");
        proj.description = Some("project".into());
        lib.insert(BlockScope::User, user).unwrap();
        lib.insert(BlockScope::Project, proj).unwrap();
        let found = lib.get("WIN").unwrap();
        assert_eq!(found.description.as_deref(), Some("project"));
    }

    #[test]
    fn empty_name_rejected() {
        let mut lib = BlockLibrary::default();
        assert!(lib.insert(BlockScope::Project, Block::new("")).is_err());
    }
}
