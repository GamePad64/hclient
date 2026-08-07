// Реализация — Task 2.
#[derive(Debug)]
pub struct FuturesIo<S> {
    #[expect(
        dead_code,
        reason = "поле читает impl hyper::rt::Read/Write в Task 2 — до тех пор struct пустой снаружи"
    )]
    pub(crate) inner: S,
}
