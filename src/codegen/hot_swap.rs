//! Verified, lease-safe hot swapping for in-process JIT functions.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use sha2::{Digest, Sha256};

use crate::codegen::llvm_orc::{JitValue, OrcJitEngine, OrcJitModule};
use crate::error::CodegenError;
use crate::ir::function::IrFunction;
use crate::ir::module::IrModule;

type ProgramGatewayMetadata = (BTreeMap<String, String>, BTreeMap<String, AbiFingerprint>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbiFingerprint {
    /// Stable compatibility prefix derived from the first eight SHA-256 bytes.
    ///
    /// This preserves the original public `u64` field for callers that use it
    /// in logs or cache keys. ABI verification uses the complete digest and
    /// canonical signature below.
    pub hash: u64,
    pub sha256: String,
    pub signature: String,
}

impl AbiFingerprint {
    pub fn for_function(function: &IrFunction) -> Self {
        let params = function
            .params
            .iter()
            .map(|param| param.ty.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let signature = format!("({}) -> {}", params, function.return_ty);
        let mut hasher = Sha256::new();
        hasher.update(b"iris-abi-v1\0");
        hash_framed(&mut hasher, function.name.as_bytes());
        hash_framed(&mut hasher, signature.as_bytes());
        let digest: [u8; 32] = hasher.finalize().into();
        let hash = u64::from_be_bytes(
            digest[..8]
                .try_into()
                .expect("SHA-256 digest always contains eight prefix bytes"),
        );
        let sha256 = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        Self {
            hash,
            sha256,
            signature,
        }
    }
}

fn hash_framed(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

struct Generation {
    id: u64,
    function_name: String,
    module: OrcJitModule,
}

struct DispatchSlot {
    abi: AbiFingerprint,
    current: Arc<Generation>,
    history: VecDeque<Arc<Generation>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramGateway {
    pub name: String,
    pub symbol: String,
}

impl ProgramGateway {
    pub fn new(name: impl Into<String>, symbol: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            symbol: symbol.into(),
        }
    }
}

struct ProgramGeneration {
    id: u64,
    module: OrcJitModule,
    gateways: BTreeMap<String, String>,
}

struct ProgramDispatchSlot {
    gateway_abis: BTreeMap<String, AbiFingerprint>,
    current: Arc<ProgramGeneration>,
    history: VecDeque<Arc<ProgramGeneration>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramSwapReceipt {
    pub program_name: String,
    pub generation: u64,
    pub replaced_generation: Option<u64>,
    pub gateway_abis: BTreeMap<String, AbiFingerprint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwapReceipt {
    pub function_name: String,
    pub generation: u64,
    pub replaced_generation: Option<u64>,
    pub abi: AbiFingerprint,
}

/// A call lease pins its generation until the call finishes. Replacing a slot
/// never invalidates machine code that an in-flight caller may still execute.
#[derive(Clone)]
pub struct HotSwapLease {
    generation: Arc<Generation>,
}

/// A program lease observes one coherent module generation across every
/// gateway. An install cannot expose a mixture of old and new entry points.
#[derive(Clone)]
pub struct ProgramLease {
    generation: Arc<ProgramGeneration>,
}

impl ProgramLease {
    pub fn generation(&self) -> u64 {
        self.generation.id
    }

    fn symbol(&self, gateway: &str) -> Result<&str, CodegenError> {
        self.generation
            .gateways
            .get(gateway)
            .map(String::as_str)
            .ok_or_else(|| CodegenError::Unsupported {
                backend: "hot-swap".into(),
                detail: format!("program generation has no gateway '{gateway}'"),
            })
    }

    pub fn call_zero_arg(&self, gateway: &str) -> Result<JitValue, CodegenError> {
        self.generation.module.call_zero_arg(self.symbol(gateway)?)
    }

    pub fn call_i64_0(&self, gateway: &str) -> Result<i64, CodegenError> {
        self.generation.module.call_i64_0(self.symbol(gateway)?)
    }

    pub fn call_i64_1(&self, gateway: &str, value: i64) -> Result<i64, CodegenError> {
        self.generation
            .module
            .call_i64_1(self.symbol(gateway)?, value)
    }

    pub fn call_str_1(&self, gateway: &str, value: &str) -> Result<String, CodegenError> {
        self.generation
            .module
            .call_str_1(self.symbol(gateway)?, value)
    }
}

impl HotSwapLease {
    pub fn generation(&self) -> u64 {
        self.generation.id
    }

    pub fn call_zero_arg(&self) -> Result<JitValue, CodegenError> {
        self.generation
            .module
            .call_zero_arg(&self.generation.function_name)
    }

    pub fn call_i64_0(&self) -> Result<i64, CodegenError> {
        self.generation
            .module
            .call_i64_0(&self.generation.function_name)
    }

    pub fn call_i64_1(&self, value: i64) -> Result<i64, CodegenError> {
        self.generation
            .module
            .call_i64_1(&self.generation.function_name, value)
    }
}

/// Stable logical dispatch slots backed by independently removable ORC
/// generations.
pub struct HotSwapEngine {
    slots: RwLock<HashMap<String, DispatchSlot>>,
    programs: RwLock<HashMap<String, ProgramDispatchSlot>>,
    next_generation: AtomicU64,
    history_limit: usize,
}

impl HotSwapEngine {
    pub fn new() -> Self {
        Self::with_history_limit(8)
    }

    /// Construct an engine retaining at most `history_limit` generations per
    /// slot, including the active generation. Values below one are clamped to
    /// one, which disables rollback while still allowing replacement.
    pub fn with_history_limit(history_limit: usize) -> Self {
        Self {
            slots: RwLock::new(HashMap::new()),
            programs: RwLock::new(HashMap::new()),
            next_generation: AtomicU64::new(1),
            history_limit: history_limit.max(1),
        }
    }

    pub fn history_limit(&self) -> usize {
        self.history_limit
    }

    /// Compile, validate, and atomically install one function generation.
    ///
    /// The validator runs against the candidate before it is visible through
    /// the dispatch slot. Returning an error leaves the active generation
    /// untouched and drops the candidate's ORC resource tracker.
    pub fn install_verified<F>(
        &self,
        module: &IrModule,
        function_name: &str,
        validator: F,
    ) -> Result<SwapReceipt, CodegenError>
    where
        F: FnOnce(&OrcJitModule) -> Result<(), CodegenError>,
    {
        let function =
            module
                .function_by_name(function_name)
                .ok_or_else(|| CodegenError::Unsupported {
                    backend: "hot-swap".into(),
                    detail: format!("candidate does not define function '{}'", function_name),
                })?;
        let abi = AbiFingerprint::for_function(function);

        {
            let slots = self.read_slots()?;
            if let Some(slot) = slots.get(function_name) {
                ensure_abi(function_name, &slot.abi, &abi)?;
            }
        }

        let engine = OrcJitEngine::new()?;
        let candidate = engine.load_module(module)?;
        // Materialize the target symbol before validation so unresolved runtime
        // references cannot enter an active slot.
        unsafe { candidate.lookup_address(function_name)? };
        validator(&candidate)?;

        let id = self.next_generation.fetch_add(1, Ordering::Relaxed);
        let generation = Arc::new(Generation {
            id,
            function_name: function_name.to_owned(),
            module: candidate,
        });
        let mut slots = self.write_slots()?;
        let replaced_generation = if let Some(slot) = slots.get_mut(function_name) {
            // Recheck under the write lock in case another installer won the race.
            ensure_abi(function_name, &slot.abi, &abi)?;
            let old = std::mem::replace(&mut slot.current, Arc::clone(&generation));
            let replaced = old.id;
            retain_generation(&mut slot.history, old, self.history_limit);
            Some(replaced)
        } else {
            slots.insert(
                function_name.to_owned(),
                DispatchSlot {
                    abi: abi.clone(),
                    current: Arc::clone(&generation),
                    history: VecDeque::new(),
                },
            );
            None
        };

        Ok(SwapReceipt {
            function_name: function_name.to_owned(),
            generation: id,
            replaced_generation,
            abi,
        })
    }

    pub fn lease(&self, function_name: &str) -> Result<HotSwapLease, CodegenError> {
        let slots = self.read_slots()?;
        let slot = slots
            .get(function_name)
            .ok_or_else(|| CodegenError::Unsupported {
                backend: "hot-swap".into(),
                detail: format!("no active generation for function '{}'", function_name),
            })?;
        Ok(HotSwapLease {
            generation: Arc::clone(&slot.current),
        })
    }

    pub fn rollback(&self, function_name: &str) -> Result<SwapReceipt, CodegenError> {
        let expected_generation =
            self.active_generation(function_name)
                .ok_or_else(|| CodegenError::Unsupported {
                    backend: "hot-swap".into(),
                    detail: format!("no active generation for function '{}'", function_name),
                })?;
        self.rollback_generation(function_name, expected_generation)
    }

    /// Roll back only when `expected_generation` is still the active one.
    ///
    /// Callers should pass the generation from the [`SwapReceipt`] they are
    /// rejecting. If a newer installer has already replaced it, this method
    /// fails without changing either the current or previous generation.
    pub fn rollback_generation(
        &self,
        function_name: &str,
        expected_generation: u64,
    ) -> Result<SwapReceipt, CodegenError> {
        let mut slots = self.write_slots()?;
        let slot = slots
            .get_mut(function_name)
            .ok_or_else(|| CodegenError::Unsupported {
                backend: "hot-swap".into(),
                detail: format!("no active generation for function '{}'", function_name),
            })?;
        if slot.current.id != expected_generation {
            return Err(CodegenError::Unsupported {
                backend: "hot-swap".into(),
                detail: format!(
                    "refusing stale rollback for function '{}': expected active generation {}, actual active generation {}",
                    function_name, expected_generation, slot.current.id
                ),
            });
        }
        let previous = slot
            .history
            .pop_front()
            .ok_or_else(|| CodegenError::Unsupported {
                backend: "hot-swap".into(),
                detail: format!(
                    "function '{}' has no generation to roll back to",
                    function_name
                ),
            })?;
        let replaced = std::mem::replace(&mut slot.current, previous);
        let active = slot.current.id;
        let replaced_id = replaced.id;
        retain_generation(&mut slot.history, replaced, self.history_limit);
        Ok(SwapReceipt {
            function_name: function_name.to_owned(),
            generation: active,
            replaced_generation: Some(replaced_id),
            abi: slot.abi.clone(),
        })
    }

    pub fn active_generation(&self, function_name: &str) -> Option<u64> {
        self.slots
            .read()
            .ok()
            .and_then(|slots| slots.get(function_name).map(|slot| slot.current.id))
    }

    /// Activate a retained historical function generation. The rejected
    /// active generation remains in the bounded history and existing leases
    /// continue to pin their machine code.
    pub fn rollback_to_generation(
        &self,
        function_name: &str,
        expected_generation: u64,
        target_generation: u64,
    ) -> Result<SwapReceipt, CodegenError> {
        let mut slots = self.write_slots()?;
        let slot = slots
            .get_mut(function_name)
            .ok_or_else(|| missing_slot(function_name))?;
        ensure_expected_generation(function_name, expected_generation, slot.current.id)?;
        let index = slot
            .history
            .iter()
            .position(|generation| generation.id == target_generation)
            .ok_or_else(|| CodegenError::Unsupported {
                backend: "hot-swap".into(),
                detail: format!(
                    "function '{function_name}' does not retain generation {target_generation}"
                ),
            })?;
        let target = slot.history.remove(index).expect("history index was found");
        let rejected = std::mem::replace(&mut slot.current, target);
        retain_generation(&mut slot.history, rejected, self.history_limit);
        Ok(SwapReceipt {
            function_name: function_name.to_owned(),
            generation: slot.current.id,
            replaced_generation: Some(expected_generation),
            abi: slot.abi.clone(),
        })
    }

    pub fn retained_generations(&self, function_name: &str) -> Vec<u64> {
        self.slots
            .read()
            .ok()
            .and_then(|slots| {
                slots.get(function_name).map(|slot| {
                    std::iter::once(slot.current.id)
                        .chain(slot.history.iter().map(|generation| generation.id))
                        .collect()
                })
            })
            .unwrap_or_default()
    }

    /// Validate and atomically publish every declared gateway from one ORC
    /// module. Materialization and validation finish before the dispatch-table
    /// write lock is acquired.
    pub fn install_program_verified<F>(
        &self,
        module: &IrModule,
        program_name: &str,
        gateways: &[ProgramGateway],
        validator: F,
    ) -> Result<ProgramSwapReceipt, CodegenError>
    where
        F: FnOnce(&OrcJitModule) -> Result<(), CodegenError>,
    {
        let (gateway_symbols, gateway_abis) = program_gateway_metadata(module, gateways)?;
        {
            let programs = self.read_programs()?;
            if let Some(slot) = programs.get(program_name) {
                ensure_program_abi(program_name, &slot.gateway_abis, &gateway_abis)?;
            }
        }

        let engine = OrcJitEngine::new()?;
        let candidate = engine.load_module(module)?;
        candidate.materialize_symbols(gateway_symbols.values().map(String::as_str))?;
        validator(&candidate)?;

        let id = self.next_generation.fetch_add(1, Ordering::Relaxed);
        let generation = Arc::new(ProgramGeneration {
            id,
            module: candidate,
            gateways: gateway_symbols,
        });
        let mut programs = self.write_programs()?;
        let replaced_generation = if let Some(slot) = programs.get_mut(program_name) {
            ensure_program_abi(program_name, &slot.gateway_abis, &gateway_abis)?;
            let old = std::mem::replace(&mut slot.current, Arc::clone(&generation));
            let replaced = old.id;
            retain_generation(&mut slot.history, old, self.history_limit);
            Some(replaced)
        } else {
            programs.insert(
                program_name.to_owned(),
                ProgramDispatchSlot {
                    gateway_abis: gateway_abis.clone(),
                    current: Arc::clone(&generation),
                    history: VecDeque::new(),
                },
            );
            None
        };
        Ok(ProgramSwapReceipt {
            program_name: program_name.to_owned(),
            generation: id,
            replaced_generation,
            gateway_abis,
        })
    }

    pub fn lease_program(&self, program_name: &str) -> Result<ProgramLease, CodegenError> {
        let programs = self.read_programs()?;
        let slot = programs
            .get(program_name)
            .ok_or_else(|| CodegenError::Unsupported {
                backend: "hot-swap".into(),
                detail: format!("no active generation for program '{program_name}'"),
            })?;
        Ok(ProgramLease {
            generation: Arc::clone(&slot.current),
        })
    }

    pub fn active_program_generation(&self, program_name: &str) -> Option<u64> {
        self.programs
            .read()
            .ok()
            .and_then(|programs| programs.get(program_name).map(|slot| slot.current.id))
    }

    pub fn retained_program_generations(&self, program_name: &str) -> Vec<u64> {
        self.programs
            .read()
            .ok()
            .and_then(|programs| {
                programs.get(program_name).map(|slot| {
                    std::iter::once(slot.current.id)
                        .chain(slot.history.iter().map(|generation| generation.id))
                        .collect()
                })
            })
            .unwrap_or_default()
    }

    pub fn rollback_program_generation(
        &self,
        program_name: &str,
        expected_generation: u64,
    ) -> Result<ProgramSwapReceipt, CodegenError> {
        let target_generation = {
            let programs = self.read_programs()?;
            let slot = programs
                .get(program_name)
                .ok_or_else(|| CodegenError::Unsupported {
                    backend: "hot-swap".into(),
                    detail: format!("no active generation for program '{program_name}'"),
                })?;
            slot.history
                .front()
                .map(|generation| generation.id)
                .ok_or_else(|| CodegenError::Unsupported {
                    backend: "hot-swap".into(),
                    detail: format!("program '{program_name}' has no generation to roll back to"),
                })?
        };
        self.rollback_program_to_generation(program_name, expected_generation, target_generation)
    }

    pub fn rollback_program_to_generation(
        &self,
        program_name: &str,
        expected_generation: u64,
        target_generation: u64,
    ) -> Result<ProgramSwapReceipt, CodegenError> {
        let mut programs = self.write_programs()?;
        let slot = programs
            .get_mut(program_name)
            .ok_or_else(|| CodegenError::Unsupported {
                backend: "hot-swap".into(),
                detail: format!("no active generation for program '{program_name}'"),
            })?;
        ensure_expected_generation(program_name, expected_generation, slot.current.id)?;
        let index = slot
            .history
            .iter()
            .position(|generation| generation.id == target_generation)
            .ok_or_else(|| CodegenError::Unsupported {
                backend: "hot-swap".into(),
                detail: format!(
                    "program '{program_name}' does not retain generation {target_generation}"
                ),
            })?;
        let target = slot.history.remove(index).expect("history index was found");
        let rejected = std::mem::replace(&mut slot.current, target);
        retain_generation(&mut slot.history, rejected, self.history_limit);
        Ok(ProgramSwapReceipt {
            program_name: program_name.to_owned(),
            generation: slot.current.id,
            replaced_generation: Some(expected_generation),
            gateway_abis: slot.gateway_abis.clone(),
        })
    }

    fn read_slots(
        &self,
    ) -> Result<std::sync::RwLockReadGuard<'_, HashMap<String, DispatchSlot>>, CodegenError> {
        self.slots.read().map_err(|_| lock_error())
    }

    fn write_slots(
        &self,
    ) -> Result<std::sync::RwLockWriteGuard<'_, HashMap<String, DispatchSlot>>, CodegenError> {
        self.slots.write().map_err(|_| lock_error())
    }

    fn read_programs(
        &self,
    ) -> Result<std::sync::RwLockReadGuard<'_, HashMap<String, ProgramDispatchSlot>>, CodegenError>
    {
        self.programs.read().map_err(|_| lock_error())
    }

    fn write_programs(
        &self,
    ) -> Result<std::sync::RwLockWriteGuard<'_, HashMap<String, ProgramDispatchSlot>>, CodegenError>
    {
        self.programs.write().map_err(|_| lock_error())
    }
}

impl Default for HotSwapEngine {
    fn default() -> Self {
        Self::new()
    }
}

fn ensure_abi(
    function_name: &str,
    active: &AbiFingerprint,
    candidate: &AbiFingerprint,
) -> Result<(), CodegenError> {
    if active == candidate {
        Ok(())
    } else {
        Err(CodegenError::Unsupported {
            backend: "hot-swap".into(),
            detail: format!(
                "ABI mismatch for '{}': active {} (sha256:{}), candidate {} (sha256:{})",
                function_name,
                active.signature,
                active.sha256,
                candidate.signature,
                candidate.sha256
            ),
        })
    }
}

fn retain_generation<T>(history: &mut VecDeque<Arc<T>>, generation: Arc<T>, limit: usize) {
    history.push_front(generation);
    while history.len() + 1 > limit {
        history.pop_back();
    }
}

fn missing_slot(function_name: &str) -> CodegenError {
    CodegenError::Unsupported {
        backend: "hot-swap".into(),
        detail: format!("no active generation for function '{function_name}'"),
    }
}

fn ensure_expected_generation(
    slot_name: &str,
    expected: u64,
    actual: u64,
) -> Result<(), CodegenError> {
    if expected == actual {
        Ok(())
    } else {
        Err(CodegenError::Unsupported {
            backend: "hot-swap".into(),
            detail: format!(
                "refusing stale rollback for '{slot_name}': expected active generation {expected}, actual active generation {actual}"
            ),
        })
    }
}

fn program_gateway_metadata(
    module: &IrModule,
    gateways: &[ProgramGateway],
) -> Result<ProgramGatewayMetadata, CodegenError> {
    if gateways.is_empty() {
        return Err(CodegenError::Unsupported {
            backend: "hot-swap".into(),
            detail: "a program must declare at least one gateway".into(),
        });
    }
    let mut symbols = BTreeMap::new();
    let mut abis = BTreeMap::new();
    for gateway in gateways {
        if gateway.name.is_empty() || gateway.symbol.is_empty() {
            return Err(CodegenError::Unsupported {
                backend: "hot-swap".into(),
                detail: "program gateway names and symbols must not be empty".into(),
            });
        }
        let function =
            module
                .function_by_name(&gateway.symbol)
                .ok_or_else(|| CodegenError::Unsupported {
                    backend: "hot-swap".into(),
                    detail: format!(
                        "candidate does not define gateway symbol '{}'",
                        gateway.symbol
                    ),
                })?;
        if symbols
            .insert(gateway.name.clone(), gateway.symbol.clone())
            .is_some()
        {
            return Err(CodegenError::Unsupported {
                backend: "hot-swap".into(),
                detail: format!("duplicate program gateway '{}'", gateway.name),
            });
        }
        abis.insert(gateway.name.clone(), AbiFingerprint::for_function(function));
    }
    Ok((symbols, abis))
}

fn ensure_program_abi(
    program_name: &str,
    active: &BTreeMap<String, AbiFingerprint>,
    candidate: &BTreeMap<String, AbiFingerprint>,
) -> Result<(), CodegenError> {
    if active == candidate {
        Ok(())
    } else {
        Err(CodegenError::Unsupported {
            backend: "hot-swap".into(),
            detail: format!("gateway ABI mismatch for program '{program_name}'"),
        })
    }
}

fn lock_error() -> CodegenError {
    CodegenError::Unsupported {
        backend: "hot-swap".into(),
        detail: "hot-swap dispatch table lock was poisoned".into(),
    }
}
