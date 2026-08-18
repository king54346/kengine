pub fn probe() -> String {
    let mut context = boa_engine::Context::default();
    let source = boa_engine::Source::from_bytes("1 + 2 * 3");
    match context.eval(source) {
        Ok(value) => format!("{value:?}"),
        Err(error) => format!("error: {error}"),
    }
}

#[cfg(test)]
mod t {
    #[test]
    fn probe_works() {
        println!("{}", super::probe());
    }
}
