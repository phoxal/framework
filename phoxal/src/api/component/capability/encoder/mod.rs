pub mod v1;

contract! {
    pub enum Sample {
        V1(v1::Sample),
    }
}
