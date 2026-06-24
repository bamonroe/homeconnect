//! Generated cereal capnp bindings (compiled from vendor/cereal by build.rs
//! into OUT_DIR). Each submodule is the `include!`d generated file; the parent
//! module name `cereal` matches `default_parent_module` in build.rs so the
//! cross-references between schemas resolve.
#![allow(clippy::all)]
#![allow(warnings)]

pub mod log_capnp {
    include!(concat!(env!("OUT_DIR"), "/log_capnp.rs"));
}
pub mod car_capnp {
    include!(concat!(env!("OUT_DIR"), "/car_capnp.rs"));
}
pub mod custom_capnp {
    include!(concat!(env!("OUT_DIR"), "/custom_capnp.rs"));
}
pub mod legacy_capnp {
    include!(concat!(env!("OUT_DIR"), "/legacy_capnp.rs"));
}
