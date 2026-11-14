/// Phase 90b: Exhaustiveness checking pass.
///
/// Scans all `SwitchVariant` instructions and verifies that every variant
/// of the enum type is covered (or a `default_block` is present).
/// Reports a `PassError::TypeError` for non-exhaustive matches.
use crate::error::PassError;
use crate::ir::instr::IrInstr;
use crate::ir::module::IrModule;
use crate::ir::types::IrType;
use crate::pass::Pass;

pub struct ExhaustivePass;

impl Pass for ExhaustivePass {
    fn name(&self) -> &'static str {
        "exhaustive"
    }

    fn run(&mut self, module: &mut IrModule) -> Result<(), PassError> {
        for func in module.functions() {
            for (bi, block) in func.blocks().iter().enumerate() {
                for (ii, instr) in block.instrs.iter().enumerate() {
                    if let IrInstr::SwitchVariant {
                        scrutinee,
                        arms,
                        default_block,
                    } = instr
                    {
                        if default_block.is_some() {
                            // A default arm covers all unspecified variants.
                            continue;
                        }
                        // Look up the enum type from the scrutinee's value type.
                        let enum_ty = func.value_type(*scrutinee);
                        let (enum_name, variant_names) = match enum_ty {
                            Some(IrType::Enum { name, variants }) => {
                                (name.clone(), variants.clone())
                            }
                            _ => continue, // can't determine — skip
                        };
                        let num_variants = variant_names.len();
                        let covered: std::collections::HashSet<usize> =
                            arms.iter().map(|(idx, _)| *idx).collect();
                        let missing: Vec<usize> =
                            (0..num_variants).filter(|v| !covered.contains(v)).collect();
                        if !missing.is_empty() {
                            let names: Vec<String> = missing
                                .iter()
                                .map(|i| format!("`{}.{}`", enum_name, variant_names[*i]))
                                .collect();
                            let mut diag = crate::diagnostics::Diagnostic::error(format!(
                                "this `when` does not handle {}",
                                names.join(", ")
                            ))
                            .with_help("add an arm for each, or a `_ =>` arm for the rest");
                            let at = func
                                .get_instr_span(bi as u32, ii)
                                .or_else(|| func.debug_span(bi as u32, ii))
                                .or_else(|| func.span_table.get_for_val(*scrutinee));
                            if let Some(b) = at {
                                diag = diag.with_primary_span(b, b + 1, None);
                            }
                            return Err(PassError::rich("E0210", diag));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
