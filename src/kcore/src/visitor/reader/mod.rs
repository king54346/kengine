pub mod ascii;
pub mod binary;

use crate::{
    pool::Handle,
    visitor::{error::VisitError, field::Field, Visitor, VisitorNode},
};

pub trait Reader {
    fn read_field(&mut self) -> Result<Field, VisitError>;
    fn read_node(&mut self, visitor: &mut Visitor) -> Result<Handle<VisitorNode>, VisitError>;
    fn read(&mut self) -> Result<Visitor, VisitError>;
}
