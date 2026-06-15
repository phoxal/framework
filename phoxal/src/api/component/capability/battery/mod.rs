pub mod v1;

contract! {
    pub enum State {
        "1" => V1(v1::State),
    }
}
