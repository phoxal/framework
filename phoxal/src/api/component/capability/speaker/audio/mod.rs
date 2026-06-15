pub mod v1;

contract! {
    pub enum Audio {
        "1" => V1(v1::Audio),
    }
}
