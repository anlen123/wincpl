//! 配对地址只来自已连接的物理以太网/Wi-Fi，不跟随代理的默认出网路由。
use std::net::Ipv4Addr;

#[derive(Clone, Debug)]
struct Candidate {
    ip: Ipv4Addr,
    up: bool,
    hardware: bool,
    interface_type: u32,
    on_link_gateway: bool,
    metric: u32,
    index: u32,
}

fn select_address(candidates: &[Candidate]) -> Option<Ipv4Addr> {
    candidates
        .iter()
        .filter(|c| c.up && c.hardware && matches!(c.interface_type, 6 | 71) && c.ip.is_private())
        // 优先有本地网关的物理网卡，其次按接口 metric；排序不依赖枚举顺序。
        .min_by_key(|c| (!c.on_link_gateway, c.metric, c.index, c.ip.octets()))
        .map(|c| c.ip)
}

fn gateway_on_link(ip: Ipv4Addr, prefix: u8, gateway: Ipv4Addr) -> bool {
    if !(1..=32).contains(&prefix) || !gateway.is_private() || gateway == ip {
        return false;
    }
    let mask = u32::MAX << (32 - prefix);
    u32::from(ip) & mask == u32::from(gateway) & mask
}

/// 找不到可靠物理局域网地址时返回 None，不能用虚拟网卡或回环地址冒充。
#[cfg(windows)]
pub fn lan_ipv4() -> Option<Ipv4Addr> {
    windows_candidates().and_then(|candidates| select_address(&candidates))
}

#[cfg(windows)]
fn windows_candidates() -> Option<Vec<Candidate>> {
    use std::mem::size_of;
    use windows::Win32::{
        Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_SUCCESS},
        NetworkManagement::{
            IpHelper::{
                GetAdaptersAddresses, GetIfEntry2, GAA_FLAG_INCLUDE_GATEWAYS,
                GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
                IP_ADAPTER_ADDRESSES_LH, MIB_IF_ROW2,
            },
            Ndis::IfOperStatusUp,
        },
        Networking::WinSock::{IpDadStatePreferred, AF_INET},
    };

    let flags = GAA_FLAG_INCLUDE_GATEWAYS
        | GAA_FLAG_SKIP_ANYCAST
        | GAA_FLAG_SKIP_MULTICAST
        | GAA_FLAG_SKIP_DNS_SERVER;
    let mut bytes = 15_000_u32;
    // Vec<usize> 保证 API 结构体所需的指针对齐；缓冲区活到遍历结束。
    for _ in 0..3 {
        if bytes as usize > 1024 * 1024 {
            return None;
        }
        let mut buffer = vec![0_usize; (bytes as usize).div_ceil(size_of::<usize>())];
        let head = buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        let result =
            unsafe { GetAdaptersAddresses(AF_INET.0.into(), flags, None, Some(head), &mut bytes) };
        if result == ERROR_BUFFER_OVERFLOW.0 {
            continue;
        }
        if result != ERROR_SUCCESS.0 {
            return None;
        }
        let mut candidates = Vec::new();
        let mut current = head;
        // 所有链表指针来自 GetAdaptersAddresses 写入的有效缓冲区。
        while !current.is_null() {
            let adapter = unsafe { &*current };
            let mut row = MIB_IF_ROW2 {
                InterfaceLuid: adapter.Luid,
                ..Default::default()
            };
            let row_ok = unsafe { GetIfEntry2(&mut row) } == ERROR_SUCCESS;
            // MIB_IF_ROW2 的 bit 0 是 HardwareInterface，而不是靠网卡名称猜测。
            let hardware = row_ok && row.InterfaceAndOperStatusFlags._bitfield & 1 != 0;
            if hardware && adapter.OperStatus == IfOperStatusUp && matches!(adapter.IfType, 6 | 71)
            {
                let mut gateways = Vec::new();
                let mut gateway = adapter.FirstGatewayAddress;
                while !gateway.is_null() {
                    let entry = unsafe { &*gateway };
                    if let Some(ip) = socket_ipv4(&entry.Address) {
                        gateways.push(ip);
                    }
                    gateway = entry.Next;
                }
                let mut unicast = adapter.FirstUnicastAddress;
                while !unicast.is_null() {
                    let entry = unsafe { &*unicast };
                    if entry.DadState == IpDadStatePreferred && entry.PreferredLifetime > 0 {
                        if let Some(ip) = socket_ipv4(&entry.Address) {
                            candidates.push(Candidate {
                                ip,
                                up: true,
                                hardware,
                                interface_type: adapter.IfType,
                                on_link_gateway: gateways
                                    .iter()
                                    .any(|g| gateway_on_link(ip, entry.OnLinkPrefixLength, *g)),
                                metric: adapter.Ipv4Metric,
                                index: row.InterfaceIndex,
                            });
                        }
                    }
                    unicast = entry.Next;
                }
            }
            current = adapter.Next;
        }
        return Some(candidates);
    }
    None
}

#[cfg(windows)]
fn socket_ipv4(address: &windows::Win32::Networking::WinSock::SOCKET_ADDRESS) -> Option<Ipv4Addr> {
    use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};
    if address.lpSockaddr.is_null()
        || address.iSockaddrLength < std::mem::size_of::<SOCKADDR_IN>() as i32
    {
        return None;
    }
    let socket = unsafe { std::ptr::read_unaligned(address.lpSockaddr.cast::<SOCKADDR_IN>()) };
    if socket.sin_family != AF_INET {
        return None;
    }
    // Windows s_addr 在内存里是网络字节序，直接读取原始四个字节。
    Some(Ipv4Addr::from(
        unsafe { socket.sin_addr.S_un.S_addr }.to_ne_bytes(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn physical(ip: &str) -> Candidate {
        Candidate {
            ip: ip.parse().unwrap(),
            up: true,
            hardware: true,
            interface_type: 6,
            on_link_gateway: true,
            metric: 25,
            index: 1,
        }
    }

    #[test]
    fn ignores_proxy_default_route_and_private_virtual_adapters() {
        let real = physical("192.168.5.18");
        let mut tun = physical("198.18.0.1");
        tun.hardware = false;
        tun.metric = 0;
        let mut vmware = physical("192.168.159.1");
        vmware.hardware = false;
        let mut zerotier = physical("10.243.189.202");
        zerotier.hardware = false;
        assert_eq!(
            select_address(&[tun, vmware, zerotier, real]),
            Some(Ipv4Addr::new(192, 168, 5, 18))
        );
    }

    #[test]
    fn rejects_unusable_and_non_lan_addresses() {
        for ip in [
            "127.0.0.1",
            "0.0.0.0",
            "169.254.1.2",
            "198.18.0.1",
            "198.19.1.1",
            "8.8.8.8",
        ] {
            assert_eq!(select_address(&[physical(ip)]), None, "{ip}");
        }
        let mut disconnected = physical("192.168.1.2");
        disconnected.up = false;
        assert_eq!(select_address(&[disconnected]), None);
        let mut tunnel = physical("10.0.0.1");
        tunnel.interface_type = 131;
        assert_eq!(select_address(&[tunnel]), None);
    }

    #[test]
    fn supports_wifi_and_all_private_ranges() {
        for ip in ["10.0.0.8", "172.16.5.9", "192.168.5.18"] {
            let mut wifi = physical(ip);
            wifi.interface_type = 71;
            assert_eq!(select_address(&[wifi]), Some(ip.parse().unwrap()));
        }
    }

    #[test]
    fn prefers_local_gateway_then_metric_independent_of_order() {
        let real = physical("192.168.5.18");
        let mut isolated = physical("10.1.1.2");
        isolated.on_link_gateway = false;
        isolated.metric = 0;
        assert_eq!(
            select_address(&[isolated.clone(), real.clone()]),
            Some(real.ip)
        );
        assert_eq!(select_address(&[real.clone(), isolated]), Some(real.ip));
        let mut wifi = physical("192.168.5.19");
        wifi.interface_type = 71;
        wifi.metric = 10;
        assert_eq!(select_address(&[real, wifi.clone()]), Some(wifi.ip));
    }

    #[test]
    fn no_virtual_or_loopback_fallback() {
        assert_eq!(select_address(&[]), None);
        let mut virtual_adapter = physical("192.168.56.1");
        virtual_adapter.hardware = false;
        assert_eq!(select_address(&[virtual_adapter]), None);
    }

    #[test]
    fn validates_gateway_subnet_and_prefix() {
        let ip = Ipv4Addr::new(192, 168, 5, 18);
        assert!(gateway_on_link(ip, 24, Ipv4Addr::new(192, 168, 5, 1)));
        assert!(!gateway_on_link(ip, 24, Ipv4Addr::new(198, 18, 0, 2)));
        assert!(!gateway_on_link(ip, 24, Ipv4Addr::new(192, 168, 6, 1)));
        assert!(!gateway_on_link(ip, 0, Ipv4Addr::new(192, 168, 5, 1)));
        assert!(!gateway_on_link(ip, 33, Ipv4Addr::new(192, 168, 5, 1)));
        assert!(!gateway_on_link(ip, 32, ip));
    }

    #[cfg(windows)]
    #[test]
    fn enumerates_windows_adapters() {
        assert!(windows_candidates().is_some());
    }
}
