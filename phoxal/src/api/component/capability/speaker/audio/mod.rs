pub mod v1;

contract! {
    pub enum Audio {
        V1(v1::Audio),
    }
}
