pub(crate) mod component;
pub(crate) mod drive;
pub(crate) mod frame;
pub(crate) mod joint;
pub(crate) mod localize;
pub(crate) mod map;
pub(crate) mod motion;
pub(crate) mod navigation;
pub(crate) mod odometry;
pub(crate) mod perception;
pub(crate) mod safety;
pub(crate) mod video;

phoxal_macros::protocol_fragment_group! {
    fragments {
        component;
        drive;
        frame;
        joint;
        localize;
        map;
        motion;
        navigation;
        odometry;
        perception;
        safety;
        video;
    }
}
