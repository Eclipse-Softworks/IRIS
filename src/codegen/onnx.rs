//! ONNX text export for `GraphIr`: the model `--emit onnx-binary` writes,
//! in protobuf's text format (see `onnx_binary`), so it can be read, diffed
//! and loaded back with `google.protobuf.text_format.Parse`.

pub use crate::codegen::onnx_binary::{emit_onnx_text, emit_onnx_text_with};
