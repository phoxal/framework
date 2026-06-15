pub mod v1;

contract! {
    #[derive(Eq)]
    pub enum State {
        V1(v1::State),
    }
}
