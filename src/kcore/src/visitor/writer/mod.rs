pub mod ascii;
pub mod binary;

use crate::visitor::{VisitResult, Visitor, VisitorNode, field::Field};
use std::io::Write;

pub trait Writer {
    fn write_field(&self, field: &Field, dest: &mut dyn Write) -> VisitResult;
    fn write_node(
        &self,
        visitor: &Visitor,
        node: &VisitorNode,
        hierarchy_level: usize,
        dest: &mut dyn Write,
    ) -> VisitResult;
    fn write(&self, visitor: &Visitor, dest: &mut dyn Write) -> VisitResult;
}
