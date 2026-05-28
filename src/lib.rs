use ipnetx::prefix::IpPrefix;
use std::net::Ipv4Addr;

pub fn sample() -> Option<IpPrefix<Ipv4Addr>> {
    let slash8 = Ipv4Addr::new(10, 0, 0, 0);

    if let Ok(prefix) = IpPrefix::new(slash8, 8) {
        Some(prefix)
    } else {
        None
    }
}
