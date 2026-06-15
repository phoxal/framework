pub mod v1;

contract! {
    pub enum Collision {
        V1(v1::Collision),
    }
}
