#[macro_use]
extern crate futures;

mod client;

pub use client::*;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
