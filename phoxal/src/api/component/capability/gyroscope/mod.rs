pub mod v1;

contract! {
    pub enum Sample {
        "1" => V1(v1::Sample),
    }
}
