
pub fn database( create: bool, drop: bool, list: bool){
    if create {
        println!("Creating database...");
    }
    if drop {
        println!("Dropping database...");
    }
    if list {
        println!("Listing databases...");
    }
}
