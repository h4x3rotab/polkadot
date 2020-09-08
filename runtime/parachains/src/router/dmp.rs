// Copyright 2020 Parity Technologies (UK) Ltd.
// This file is part of Polkadot.

// Polkadot is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Polkadot is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Polkadot.  If not, see <http://www.gnu.org/licenses/>.

use super::{Trait, Module, Store};
use frame_support::{StorageMap, weights::Weight, traits::Get};
use sp_runtime::traits::{BlakeTwo256, Hash as HashT, SaturatedConversion};
use primitives::v1::{Id as ParaId, DownwardMessage, InboundDownwardMessage, Hash};

impl<T: Trait> Module<T> {
	/// Enqueue a downward message to a specific recipient para.
	pub fn queue_downward_message(para: ParaId, msg: DownwardMessage) {
		let inbound = InboundDownwardMessage {
			msg,
			sent_at: <frame_system::Module<T>>::block_number(),
		};

		// obtain the new link in the MQC and update the head.
		<Self as Store>::DownwardMessageQueueHeads::mutate(para, |head| {
			let new_head = BlakeTwo256::hash_of(&(
				*head,
				inbound.sent_at,
				T::Hashing::hash_of(&inbound.msg),
			));
			*head = new_head;
		});

		<Self as Store>::DownwardMessageQueues::mutate(para, |v| {
			v.push(inbound);
		});
	}

	/// Checks if the number of processed downward messages is valid, i.e.:
	///
	/// - if there are pending messages then `processed_downward_messages` should be at least 1,
	/// - `processed_downward_messages` should not be greater than the number of pending messages.
	///
	/// Returns true if all checks have been passed.
	pub(crate) fn check_processed_downward_messages(
		para: ParaId,
		processed_downward_messages: u32,
	) -> bool {
		let dmq_length = Self::dmq_length(para);

		if dmq_length > 0 && processed_downward_messages == 0 {
			return false;
		}
		if dmq_length < processed_downward_messages {
			return false;
		}

		true
	}

	/// Prunes the specified number of messages from the downward message queue of the given para.
	pub(crate) fn prune_dmq(para: ParaId, processed_downward_messages: u32) -> Weight {
		<Self as Store>::DownwardMessageQueues::mutate(para, |q| {
			let processed_downward_messages = processed_downward_messages as usize;
			if processed_downward_messages > q.len() {
				// reaching this branch is unexpected due to the constraint established by
				// `check_processed_downward_messages`. But better be safe than sorry.
				q.clear();
			} else {
				*q = q.split_off(processed_downward_messages);
			}
		});
		T::DbWeight::get().reads_writes(1, 1)
	}

	/// Returns the Head of Message Queue Chain for the given para or `None` if there is none
	/// associated with it.
	pub(crate) fn dmq_mqc_head(para: ParaId) -> Hash {
		<Self as Store>::DownwardMessageQueueHeads::get(&para)
	}

	/// Returns the number of pending downward messages addressed to the given para.
	///
	/// Returns 0 if the para doesn't have an associated downward message queue.
	pub(crate) fn dmq_length(para: ParaId) -> u32 {
		<Self as Store>::DownwardMessageQueues::decode_len(&para)
			.unwrap_or(0)
			.saturated_into::<u32>()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::mock::{Router, new_test_ext};
	use crate::router::{
		OutgoingParas,
		tests::{
			default_genesis_config,
			run_to_block,
		},
	};
	use frame_support::StorageValue;

	#[test]
	fn scheduled_cleanup_performed() {
		let a = ParaId::from(1312);
		let b = ParaId::from(228);
		let c = ParaId::from(123);

		new_test_ext(default_genesis_config()).execute_with(|| {
			run_to_block(1, None);

			// enqueue downward messages to A, B and C.
			Router::queue_downward_message(a, vec![1, 2, 3]);
			Router::queue_downward_message(b, vec![4, 5, 6]);
			Router::queue_downward_message(c, vec![7, 8, 9]);

			Router::schedule_para_cleanup(a);

			// run to block without session change.
			run_to_block(2, None);

			assert!(!<Router as Store>::DownwardMessageQueues::get(&a).is_empty());
			assert!(!<Router as Store>::DownwardMessageQueues::get(&b).is_empty());
			assert!(!<Router as Store>::DownwardMessageQueues::get(&c).is_empty());

			Router::schedule_para_cleanup(b);

			// run to block changing the session.
			run_to_block(3, Some(vec![3]));

			assert!(<Router as Store>::DownwardMessageQueues::get(&a).is_empty());
			assert!(<Router as Store>::DownwardMessageQueues::get(&b).is_empty());
			assert!(!<Router as Store>::DownwardMessageQueues::get(&c).is_empty());

			// verify that the outgoing paras are emptied.
			assert!(OutgoingParas::get().is_empty())
		});
	}

	#[test]
	fn dmq_length_and_head_updated_properly() {
		let a = ParaId::from(1312);
		let b = ParaId::from(228);

		new_test_ext(default_genesis_config()).execute_with(|| {
			assert_eq!(Router::dmq_length(a), 0);
			assert_eq!(Router::dmq_length(b), 0);

			Router::queue_downward_message(a, vec![1, 2, 3]);

			assert_eq!(Router::dmq_length(a), 1);
			assert_eq!(Router::dmq_length(b), 0);
			assert!(!Router::dmq_mqc_head(a).is_zero());
			assert!(Router::dmq_mqc_head(b).is_zero());
		});
	}

	#[test]
	fn check_processed_downward_messages() {
		let a = ParaId::from(1312);

		new_test_ext(default_genesis_config()).execute_with(|| {
			// processed_downward_messages=0 is allowed when the DMQ is empty.
			assert!(Router::check_processed_downward_messages(a, 0));

			Router::queue_downward_message(a, vec![1, 2, 3]);
			Router::queue_downward_message(a, vec![4, 5, 6]);
			Router::queue_downward_message(a, vec![7, 8, 9]);

			// 0 doesn't pass if the DMQ has msgs.
			assert!(!Router::check_processed_downward_messages(a, 0));
			// a candidate can consume up to 3 messages
			assert!(Router::check_processed_downward_messages(a, 1));
			assert!(Router::check_processed_downward_messages(a, 2));
			assert!(Router::check_processed_downward_messages(a, 3));
			// there is no 4 messages in the queue
			assert!(!Router::check_processed_downward_messages(a, 4));
		});
	}

	#[test]
	fn dmq_pruning() {
		let a = ParaId::from(1312);

		new_test_ext(default_genesis_config()).execute_with(|| {
			assert_eq!(Router::dmq_length(a), 0);

			Router::queue_downward_message(a, vec![1, 2, 3]);
			Router::queue_downward_message(a, vec![4, 5, 6]);
			Router::queue_downward_message(a, vec![7, 8, 9]);
			assert_eq!(Router::dmq_length(a), 3);

			// pruning 0 elements shouldn't change anything.
			Router::prune_dmq(a, 0);
			assert_eq!(Router::dmq_length(a), 3);

			Router::prune_dmq(a, 2);
			assert_eq!(Router::dmq_length(a), 1);
		});
	}
}
