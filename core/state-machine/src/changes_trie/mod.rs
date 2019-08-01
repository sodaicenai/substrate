// Copyright 2017-2019 Parity Technologies (UK) Ltd.
// This file is part of Substrate.

// Substrate is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Substrate is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Substrate.  If not, see <http://www.gnu.org/licenses/>.

//! Changes trie related structures and functions.
//!
//! Changes trie is a trie built of { storage key => extrinsiscs } pairs
//! at the end of each block. For every changed storage key it contains
//! a pair, mapping key to the set of extrinsics where it has been changed.
//!
//! Optionally, every N blocks, additional level1-digest nodes are appended
//! to the changes trie, containing pairs { storage key => blocks }. For every
//! storage key that has been changed in PREVIOUS N-1 blocks (except for genesis
//! block) it contains a pair, mapping this key to the set of blocks where it
//! has been changed.
//!
//! Optionally, every N^digest_level (where digest_level > 1) blocks, additional
//! digest_level digest is created. It is built out of pairs { storage key => digest
//! block }, containing entries for every storage key that has been changed in
//! the last N*digest_level-1 blocks (except for genesis block), mapping these keys
//! to the set of lower-level digest blocks.
//!
//! Changes trie only contains the top level storage changes. Sub-level changes
//! are propagated through its storage root on the top level storage.

mod build;
mod build_iterator;
mod changes_iterator;
mod input;
mod prune;
mod storage;
mod surface_iterator;

pub use self::storage::InMemoryStorage;
pub use self::changes_iterator::{
	key_changes, key_changes_proof,
	key_changes_proof_check, key_changes_proof_check_with_db,
};
pub use self::prune::{prune, oldest_non_pruned_trie};

use std::convert::TryInto;
use hash_db::Hasher;
use crate::backend::Backend;
use num_traits::{One, Zero};
use parity_codec::{Decode, Encode};
use primitives;
use crate::changes_trie::build::prepare_input;
use crate::overlayed_changes::OverlayedChanges;
use trie::{MemoryDB, TrieDBMut, TrieMut, DBValue};

/// Changes that are made outside of extrinsics are marked with this index;
pub const NO_EXTRINSIC_INDEX: u32 = 0xffffffff;

/// Requirements for block number that can be used with changes tries.
pub trait BlockNumber:
	Send + Sync + 'static +
	::std::fmt::Display +
	Clone +
	From<u32> + TryInto<u32> + One + Zero +
	PartialEq + Ord +
	::std::ops::Add<Self, Output=Self> + ::std::ops::Sub<Self, Output=Self> +
	::std::ops::Mul<Self, Output=Self> + ::std::ops::Div<Self, Output=Self> +
	::std::ops::Rem<Self, Output=Self> +
	::std::ops::AddAssign<Self> +
	num_traits::CheckedMul + num_traits::CheckedSub +
	Decode + Encode
{}

impl<T> BlockNumber for T where T:
	Send + Sync + 'static +
	::std::fmt::Display +
	Clone +
	From<u32> + TryInto<u32> + One + Zero +
	PartialEq + Ord +
	::std::ops::Add<Self, Output=Self> + ::std::ops::Sub<Self, Output=Self> +
	::std::ops::Mul<Self, Output=Self> + ::std::ops::Div<Self, Output=Self> +
	::std::ops::Rem<Self, Output=Self> +
	::std::ops::AddAssign<Self> +
	num_traits::CheckedMul + num_traits::CheckedSub +
	Decode + Encode,
{}

/// Block identifier that could be used to determine fork of this block.
#[derive(Debug)]
pub struct AnchorBlockId<Hash: ::std::fmt::Debug, Number: BlockNumber> {
	/// Hash of this block.
	pub hash: Hash,
	/// Number of this block.
	pub number: Number,
}

/// Changes tries state at some block.
pub struct State<'a, H, Number> {
	/// Configuration that is active at given block.
	pub config: Configuration,
	/// Configuration activation block number. Zero if it is the first coonfiguration on the chain,
	/// or number of the block that have emit NewConfiguration signal (thus activating configuration
	/// starting from the **next** block).
	pub config_activation_block: Number,
	/// Underlying changes tries storage reference.
	pub storage: &'a dyn Storage<H, Number>,
}

/// Changes trie storage. Provides access to trie roots and trie nodes.
pub trait RootsStorage<H: Hasher, Number: BlockNumber>: Send + Sync {
	/// Resolve hash of the block into anchor.
	fn build_anchor(&self, hash: H::Out) -> Result<AnchorBlockId<H::Out, Number>, String>;
	/// Get changes trie root for the block with given number which is an ancestor (or the block
	/// itself) of the anchor_block (i.e. anchor_block.number >= block).
	fn root(&self, anchor: &AnchorBlockId<H::Out, Number>, block: Number) -> Result<Option<H::Out>, String>;
}

/// Changes trie storage. Provides access to trie roots and trie nodes.
pub trait Storage<H: Hasher, Number: BlockNumber>: RootsStorage<H, Number> {
	/// Casts from self reference to RootsStorage reference.
	fn as_roots_storage(&self) -> &dyn RootsStorage<H, Number>;
	/// Get a trie node.
	fn get(&self, key: &H::Out, prefix: &[u8]) -> Result<Option<DBValue>, String>;
}

/// Changes trie storage -> trie backend essence adapter.
pub struct TrieBackendStorageAdapter<'a, H: Hasher, Number: BlockNumber>(pub &'a dyn Storage<H, Number>);

impl<'a, H: Hasher, N: BlockNumber> crate::TrieBackendStorage<H> for TrieBackendStorageAdapter<'a, H, N> {
	type Overlay = trie::MemoryDB<H>;

	fn get(&self, key: &H::Out, prefix: &[u8]) -> Result<Option<DBValue>, String> {
		self.0.get(key, prefix)
	}
}

/// Changes trie configuration.
pub type Configuration = primitives::ChangesTrieConfiguration;

/// Blocks range where configuration has been constant.
#[derive(Clone)]
pub struct ConfigurationRange<'a, N> {
	/// Active configuration.
	pub config: &'a Configuration,
	/// Zero block of this configuration. The configuration is active starting from the next block.
	pub zero: N,
	/// End block of this configuration. It is the last block where configuration has been active.
	pub end: Option<N>,
}

impl<'a, H, Number> State<'a, H, Number> {
	/// Create state with given config and storage.
	pub fn new(
		config: Configuration,
		config_activation_block: Number,
		storage: &'a dyn Storage<H, Number>,
	) -> Self {
		Self {
			config,
			config_activation_block,
			storage,
		}
	}
}

/// Create state where changes tries are disabled.
pub fn disabled_state<'a, H, Number>() -> Option<State<'a, H, Number>> {
	None
}

/// Compute the changes trie root and transaction for given block.
/// Returns Err(()) if unknown `parent_hash` has been passed.
/// Returns Ok(None) if there's no data to perform computation.
/// Panics if background storage returns an error OR if insert to MemoryDB fails.
pub fn build_changes_trie<'a, B: Backend<H>, H: Hasher, Number: BlockNumber>(
	backend: &B,
	state: Option<&'a State<'a, H, Number>>,
	changes: &OverlayedChanges,
	parent_hash: H::Out,
) -> Result<Option<(MemoryDB<H>, H::Out)>, ()>
	where
		H::Out: Ord + 'static,
{
	// when storage isn't provided, changes tries aren't created
	let state = match state {
		Some(state) => state,
		None => return Ok(None),
	};

	// build_anchor error should not be considered fatal (passed parent_hash may be incorrect)
	let parent = state.storage.build_anchor(parent_hash).map_err(|_| ())?;

	// prepare configuration range - we already know zero block. Current block may be the end block if configuration
	// has been changed in this block
	let is_config_changed = match changes.storage(primitives::storage::well_known_keys::CHANGES_TRIE_CONFIG) {
		Some(Some(new_config)) => new_config != &state.config.encode()[..],
		Some(None) => true,
		None => false,
	};
	let config_range = ConfigurationRange {
		config: &state.config,
		zero: state.config_activation_block.clone(),
		end: if is_config_changed { Some(parent.number.clone() + One::one()) } else { None },
	};

	// storage errors are considered fatal (similar to situations when runtime fetches values from storage)
	let input_pairs = prepare_input::<B, H, Number>(
		backend,
		state.storage,
		config_range,
		changes,
		&parent,
	).expect("changes trie: storage access is not allowed to fail within runtime");
	let mut root = Default::default();
	let mut mdb = MemoryDB::default();
	{
		let mut trie = TrieDBMut::<H>::new(&mut mdb, &mut root);
		for (key, value) in input_pairs.map(Into::into) {
			trie.insert(&key, &value)
				.expect("changes trie: insertion to trie is not allowed to fail within runtime");
		}
	}

	Ok(Some((mdb, root)))
}
