pub mod v1;

contract! {
    pub enum Detections {
        V1(v1::Detections),
    }
}

contract! {
    pub enum PerceptionState {
        V1(v1::PerceptionState),
    }
}
