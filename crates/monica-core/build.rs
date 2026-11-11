fn main() {
    // sqlx::migrate! must rebuild when a new migration file is added, too.
    println!("cargo:rerun-if-changed=../../migrations");
}
