//! Records the git commit this build is made from, as the environment
//! variable `ISLAND_BUILD_COMMIT`. The script is shared by the binaries.

include!("../../scripts/build_commit.rs");
