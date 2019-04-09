#[macro_use]
extern crate futures;

#[macro_use]
extern crate log;

#[cfg(test)]
#[macro_use]
extern crate maplit;

mod client;

pub use client::*;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
