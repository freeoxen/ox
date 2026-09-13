use structfs_core_store::path;

fn main() {
    let account = String::from("account");
    let _p = path!("gate", "accounts", account);
}
