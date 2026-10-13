//! Launch-scoped virtual outputs. Prepare on a blocking worker; route canonical
//! input on its owner. Remove resources from the book before dropping/joining
//! them outside the input thread and any shared-state lock.

use std::collections::{BTreeMap, BTreeSet};

use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vgames_core::manifest::Controllers;

use super::backends::{BackendError, RumbleSink, VirtualPad, VirtualPadBackend};
use super::decision::{EmulationMode, LaunchControllers, PadDecision, decide};
use super::mapping::{Mapper, PadState, Profile};
use crate::events::ControllerKind;

#[derive(Clone)]
pub struct Policy {
    /// Only the verified signed launch context supplies this configuration.
    pub manifest: Option<Controllers>,
    pub compat_layer: bool,
    pub mode: EmulationMode,
    pub mapping: Profile,
}
#[derive(Clone)]
pub struct PhysicalPad {
    pub instance_id: u32,
    pub kind: ControllerKind,
    pub player_index: Option<u16>,
    pub state: PadState,
}
struct Output {
    mapper: Mapper,
    pad: Box<dyn VirtualPad>,
}
pub struct Prepared {
    id: Uuid,
    cancel: CancellationToken,
    policy: Policy,
    outputs: BTreeMap<u32, Output>,
    notices: Vec<u32>,
}
#[derive(Debug, thiserror::Error)]
pub enum PrepareError {
    #[error("Controller preparation was cancelled.")]
    Cancelled,
}
impl Prepared {
    pub fn id(&self) -> Uuid {
        self.id
    }
    pub fn policy(&self) -> &Policy {
        &self.policy
    }
    pub fn notices(&self) -> &[u32] {
        &self.notices
    }
    pub fn emulated(&self) -> impl Iterator<Item = (u32, u16)> + '_ {
        self.outputs
            .iter()
            .map(|(id, output)| (*id, output.pad.player_index()))
    }
    /// Hot paths perform a lookup and mapping without allocation or enumeration.
    pub fn report(&mut self, id: u32, state: &PadState) -> Result<(), BackendError> {
        if let Some(output) = self.outputs.get_mut(&id) {
            output.pad.update(&output.mapper.map(state))?;
        }
        Ok(())
    }
    /// Return ownership so driver teardown runs on a blocking worker.
    pub fn disconnect(&mut self, id: u32) -> Option<Box<dyn VirtualPad>> {
        self.outputs.remove(&id).map(|output| output.pad)
    }
}

/// Creation and initial reports can touch drivers and must run on a blocking pool.
/// Driver failure changes only that pad's decision; no unsupported virtual output
/// is installed and no physical pad may be hidden until creation succeeds.
pub fn prepare(
    id: Uuid,
    policy: Policy,
    pads: &[PhysicalPad],
    backend: &dyn VirtualPadBackend,
    feedback: &dyn Fn(u32) -> RumbleSink,
    cancel: &CancellationToken,
) -> Result<Prepared, PrepareError> {
    let mut session = Prepared {
        id,
        cancel: cancel.clone(),
        policy,
        outputs: BTreeMap::new(),
        notices: Vec::new(),
    };
    let mut ordered = pads.to_vec();
    ordered.sort_by_key(|pad| (pad.player_index.unwrap_or(u16::MAX), pad.instance_id));
    let mut indexes: BTreeSet<u16> = pads.iter().filter_map(|pad| pad.player_index).collect();
    let mut seen = BTreeSet::new();
    for pad in ordered {
        if cancel.is_cancelled() {
            return Err(PrepareError::Cancelled);
        }
        if !seen.insert(pad.instance_id) {
            continue;
        }
        let decision = |available| {
            decide(
                &LaunchControllers {
                    manifest: session.policy.manifest.as_ref(),
                    compat_layer: session.policy.compat_layer,
                    mode: session.policy.mode,
                    backend_available: available,
                },
                pad.kind,
            )
        };
        match decision(true) {
            PadDecision::Emulate(target) => {
                let Some(index) = pad
                    .player_index
                    .or_else(|| (0..=u16::MAX).find(|index| !indexes.contains(index)))
                else {
                    session.notices.push(pad.instance_id);
                    continue;
                };
                let mapper = Mapper::new(pad.kind, &session.policy.mapping);
                let output = backend
                    .create(target, index, feedback(pad.instance_id))
                    .and_then(|mut output| {
                        output.update(&mapper.map(&pad.state))?;
                        Ok(output)
                    });
                match output {
                    Ok(output) => {
                        indexes.insert(index);
                        session.outputs.insert(
                            pad.instance_id,
                            Output {
                                mapper,
                                pad: output,
                            },
                        );
                    }
                    Err(error) => {
                        tracing::warn!(instance_id = pad.instance_id, %error, "virtual controller preparation failed");
                        if decision(false) == PadDecision::PassthroughWithNotice {
                            session.notices.push(pad.instance_id);
                        }
                    }
                }
            }
            PadDecision::PassthroughWithNotice => session.notices.push(pad.instance_id),
            PadDecision::Passthrough => {}
        }
    }
    if cancel.is_cancelled() {
        return Err(PrepareError::Cancelled);
    }
    Ok(session)
}

struct Slot {
    cancel: CancellationToken,
    prepared: Option<Prepared>,
}
#[derive(Default)]
pub struct Book {
    slots: BTreeMap<Uuid, Slot>,
}
impl Book {
    pub fn contains(&self, id: Uuid) -> bool {
        self.slots.contains_key(&id)
    }
    pub fn begin(&mut self, id: Uuid) -> Option<CancellationToken> {
        let std::collections::btree_map::Entry::Vacant(entry) = self.slots.entry(id) else {
            return None;
        };
        let cancel = CancellationToken::new();
        entry.insert(Slot {
            cancel: cancel.clone(),
            prepared: None,
        });
        Some(cancel)
    }
    /// A timed-out/aborted preparation must not install late. Rejected resources
    /// return to the blocking caller; never drop/join them under the book's lock.
    pub fn install(&mut self, prepared: Prepared) -> Result<(), Box<Prepared>> {
        match self.slots.get_mut(&prepared.id) {
            Some(slot)
                if !slot.cancel.is_cancelled()
                    && !prepared.cancel.is_cancelled()
                    && slot.prepared.is_none() =>
            {
                slot.prepared = Some(prepared);
                Ok(())
            }
            _ => Err(Box::new(prepared)),
        }
    }
    /// Shutdown cancels pending builders and returns outputs for off-thread teardown.
    pub fn drain(&mut self) -> Vec<Prepared> {
        std::mem::take(&mut self.slots)
            .into_values()
            .filter_map(|slot| {
                slot.cancel.cancel();
                slot.prepared
            })
            .collect()
    }
    pub fn report(&mut self, id: u32, state: &PadState) {
        for slot in self.slots.values_mut() {
            if let Some(session) = &mut slot.prepared
                && let Err(error) = session.report(id, state)
            {
                tracing::warn!(instance_id = id, %error, "virtual report failed");
            }
        }
    }
    pub fn disconnect(&mut self, id: u32) -> Vec<Box<dyn VirtualPad>> {
        self.slots
            .values_mut()
            .filter_map(|slot| slot.prepared.as_mut()?.disconnect(id))
            .collect()
    }
    pub fn session_mut(&mut self, id: Uuid) -> Option<&mut Prepared> {
        self.slots.get_mut(&id)?.prepared.as_mut()
    }
    /// Called by abort/exit. An older session cannot remove a newer session for
    /// the same package because the caller supplies the launch's unique UUID.
    pub fn take(&mut self, id: Uuid) -> Option<Prepared> {
        let slot = self.slots.remove(&id)?;
        slot.cancel.cancel();
        slot.prepared
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::super::backends::EmulatedController;
    use super::super::mapping::{Button, XReport};
    use super::*;
    use std::sync::{Arc, Mutex};
    #[derive(Default)]
    struct Trace {
        indexes: Vec<u16>,
        reports: Vec<XReport>,
        drops: usize,
        fail: bool,
    }
    struct Fake(Arc<Mutex<Trace>>);
    struct Pad {
        trace: Arc<Mutex<Trace>>,
        index: u16,
    }
    impl VirtualPadBackend for Fake {
        fn create(
            &self,
            _: EmulatedController,
            index: u16,
            _: RumbleSink,
        ) -> Result<Box<dyn VirtualPad>, BackendError> {
            let mut trace = self.0.lock().unwrap();
            if trace.fail {
                return Err(BackendError::Stopped);
            }
            trace.indexes.push(index);
            Ok(Box::new(Pad {
                trace: self.0.clone(),
                index,
            }))
        }
    }
    impl VirtualPad for Pad {
        fn update(&mut self, state: &XReport) -> Result<(), BackendError> {
            self.trace.lock().unwrap().reports.push(*state);
            Ok(())
        }
        fn player_index(&self) -> u16 {
            self.index
        }
    }
    impl Drop for Pad {
        fn drop(&mut self) {
            self.trace.lock().unwrap().drops += 1;
        }
    }
    fn policy() -> Policy {
        Policy {
            manifest: Some(Controllers {
                supported: vec![vgames_core::manifest::ControllerKind::Xinput],
                emulate_as: Some(EmulatedController::Xbox360),
            }),
            compat_layer: false,
            mode: EmulationMode::Auto,
            mapping: Profile::default(),
        }
    }
    fn pad(id: u32, index: Option<u16>) -> PhysicalPad {
        PhysicalPad {
            instance_id: id,
            kind: ControllerKind::Dualsense,
            player_index: index,
            state: PadState::default(),
        }
    }
    fn prepared(
        id: Uuid,
        policy: Policy,
        pads: &[PhysicalPad],
        trace: Arc<Mutex<Trace>>,
        cancel: &CancellationToken,
    ) -> Prepared {
        prepare(
            id,
            policy,
            pads,
            &Fake(trace),
            &|_| Arc::new(|_| {}),
            cancel,
        )
        .unwrap()
    }
    #[test]
    fn initial_held_input_player_order_duplicate_ids_and_disconnect_are_preserved() {
        let trace = Arc::new(Mutex::new(Trace::default()));
        let mut held = pad(9, Some(2));
        held.state.set(Button::South, true);
        let mut session = prepared(
            Uuid::new_v4(),
            policy(),
            &[pad(1, None), held.clone(), held],
            trace.clone(),
            &CancellationToken::new(),
        );
        assert_eq!(trace.lock().unwrap().indexes, [2, 0]);
        assert_eq!(
            trace.lock().unwrap().reports.first().unwrap().buttons,
            0x1000
        );
        assert_eq!(session.emulated().count(), 2);
        session.report(9, &PadState::default()).unwrap();
        let removed = session.disconnect(9).unwrap();
        assert_eq!(trace.lock().unwrap().drops, 0);
        drop(removed);
        assert_eq!(trace.lock().unwrap().drops, 1);
        drop(session);
        assert_eq!(trace.lock().unwrap().drops, 2);
    }
    #[test]
    fn compat_never_and_driver_failures_do_not_create_or_hide_outputs() {
        let trace = Arc::new(Mutex::new(Trace::default()));
        for policy in [
            Policy {
                compat_layer: true,
                ..policy()
            },
            Policy {
                mode: EmulationMode::Never,
                ..policy()
            },
        ] {
            let session = prepared(
                Uuid::new_v4(),
                policy,
                &[pad(1, None)],
                trace.clone(),
                &CancellationToken::new(),
            );
            assert_eq!(session.emulated().count(), 0);
            assert!(session.notices().is_empty());
        }
        assert!(trace.lock().unwrap().indexes.is_empty());
        trace.lock().unwrap().fail = true;
        let session = prepared(
            Uuid::new_v4(),
            policy(),
            &[pad(1, None)],
            trace.clone(),
            &CancellationToken::new(),
        );
        assert_eq!(session.emulated().count(), 0);
        assert_eq!(session.notices(), [1]);
    }
    #[test]
    fn late_preparation_is_rejected_and_old_exit_cannot_remove_the_new_session() {
        let trace = Arc::new(Mutex::new(Trace::default()));
        let old = Uuid::new_v4();
        let new = Uuid::new_v4();
        let mut book = Book::default();
        let cancel = book.begin(old).unwrap();
        assert!(book.begin(old).is_none());
        let late = prepared(old, policy(), &[pad(1, None)], trace.clone(), &cancel);
        assert!(book.take(old).is_none());
        assert!(cancel.is_cancelled());
        let next = book.begin(new).unwrap();
        assert!(
            book.install(prepared(
                new,
                policy(),
                &[pad(1, None)],
                trace.clone(),
                &next
            ))
            .is_ok()
        );
        let rejected = book.install(late).err().unwrap();
        assert_eq!(trace.lock().unwrap().drops, 0);
        drop(rejected);
        assert!(book.take(old).is_none());
        assert!(book.session_mut(new).is_some());
        drop(book.take(new));
        assert!(next.is_cancelled());
        assert_eq!(trace.lock().unwrap().drops, 2);
        assert!(matches!(
            prepare(
                old,
                policy(),
                &[pad(1, None)],
                &Fake(trace),
                &|_| Arc::new(|_| {}),
                &cancel
            ),
            Err(PrepareError::Cancelled)
        ));
    }
    #[test]
    fn cancelled_generation_cannot_install_if_an_identity_is_reused_and_shutdown_drains() {
        let trace = Arc::new(Mutex::new(Trace::default()));
        let id = Uuid::new_v4();
        let mut book = Book::default();
        let old = book.begin(id).unwrap();
        let late = prepared(id, policy(), &[pad(1, None)], trace.clone(), &old);
        book.take(id);
        let next = book.begin(id).unwrap();
        let rejected = book.install(late).err().unwrap();
        assert!(book.session_mut(id).is_none());
        drop(rejected);
        assert!(
            book.install(prepared(
                id,
                policy(),
                &[pad(1, None)],
                trace.clone(),
                &next
            ))
            .is_ok()
        );
        let pending = book.begin(Uuid::new_v4()).unwrap();
        let removed = book.drain();
        assert!(pending.is_cancelled());
        assert!(next.is_cancelled());
        assert!(book.session_mut(id).is_none());
        assert_eq!(trace.lock().unwrap().drops, 1);
        drop(removed);
        assert_eq!(trace.lock().unwrap().drops, 2);
    }
}
