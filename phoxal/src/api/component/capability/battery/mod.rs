pub mod v1;

contract! {
    pub enum State {
        V1(v1::State),
    }
}
