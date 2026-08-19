#![allow(clippy::manual_unwrap_or_default)]

mod visit;

use proc_macro::TokenStream;
use syn::{parse_macro_input, DeriveInput};

/// Implements `Visit` trait
///
/// User has to import `Visit`, `Visitor` and `VisitResult` to use this macro.
///
/// # Expansion
///
/// For example,
///
/// ```
/// use kcore::visitor::prelude::*;
/// #[derive(Visit)]
/// struct Foo<T: Visit> {
///     example_one: String,
///     example_two: T,
/// }
/// # fn main() {}
/// ```
///
/// would expand to something like:
///
/// ```
/// # use kcore::visitor::prelude::*;
/// # struct Foo<T> { example_one: String, example_two: T,}
/// impl<T> Visit for Foo<T> where T: Visit {
///     fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
///         let mut region = visitor.enter_region(name)?;
///         self.example_one.visit("ExampleOne", &mut region)?;
///         self.example_two.visit("ExampleTwo", &mut region)?;
///         Ok(())
///     }
/// }
/// # fn main() {}
/// ```
///
/// Field names are converted to strings using
/// [to_case(Case::UpperCamel)](https://docs.rs/convert_case/0.6.0/convert_case/enum.Case.html#variant.Pascal).
///
/// ```
/// # use kcore::visitor::prelude::*;
/// #[derive(Visit)]
/// struct Pair (usize, usize);
/// # fn main() {}
/// ```
///
/// would expand to something like:
///
/// ```
/// # use kcore::visitor::prelude::*;
/// # struct Pair (usize, usize);
/// impl Visit for Pair {
///     fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
///         let mut region = visitor.enter_region(name)?;
///         self.0.visit("0", &mut region)?;
///         self.1.visit("1", &mut region)?;
///         Ok(())
///     }
/// }
/// # fn main() {}
/// ```
///
/// ```
/// # use kcore::visitor::prelude::*;
/// #[derive(Visit)]
/// enum EnumExample { A, B(usize) }
/// # fn main() {}
/// ```
///
/// would expand to something like:
///
/// ```
/// # use kcore::visitor::prelude::*;
/// # enum EnumExample { A, B(usize) }
/// impl Visit for EnumExample {
///     fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
///         let mut region = visitor.enter_region(name)?;
///         let mut id = id(self);
///         id.visit("Id", &mut region)?;
///         if region.is_reading() {
///             *self = from_id(id)?;
///         }
///         match self {
///             EnumExample::A => (),
///             EnumExample::B(x) => { x.visit("0", &mut region)?; }
///         }
///         return Ok(());
///         fn id(me: &EnumExample) -> u32 {
///             match me {
///                 EnumExample::A => 0,
///                 EnumExample::B(_) => 1,
///             }
///         }
///         fn from_id(id: u32) -> std::result::Result<EnumExample,String> {
///             match id {
///                 0 => Ok(EnumExample::A),
///                 1 => Ok(EnumExample::B(Default::default())),
///                 _ => Err(format!("Unknown ID for type `EnumExample`: `{}`", id)),
///             }
///         }
///     }
/// }
/// # fn main() {}
/// ```
#[proc_macro_derive(Visit, attributes(visit))]
pub fn visit(input: TokenStream) -> TokenStream {
    let ast = parse_macro_input!(input as DeriveInput);
    TokenStream::from(visit::impl_visit(ast))
}

#[proc_macro]
pub fn impl_visit(input: TokenStream) -> TokenStream {
    let ast = parse_macro_input!(input as DeriveInput);
    TokenStream::from(visit::impl_visit(ast))
}
