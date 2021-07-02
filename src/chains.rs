pub mod substrate {
    #![allow(dead_code)]
    macro_rules! define_chain {
        ($name:ident => $endpoint:expr) => {
            #[derive(Debug, Clone, Copy, Eq, PartialEq)]
            pub struct $name;
            impl $name {
                pub const fn endpoint() -> &'static str { $endpoint }
            }
        };
        ($($name:ident => $endpoint:expr),+) => {
            $(define_chain!($name => $endpoint);)+
        }
    }

    define_chain! {
        Edgeware => "wss://mainnet1.edgewa.re",
        Beresheet => "wss://beresheet1.edgewa.re",
        Webb => "ws://127.0.0.1:9944"
    }
}

pub mod evm {
    use std::collections::HashMap;
    /// All Supported Chains by Webb Realyer.
    #[derive(Debug, Clone, Copy, Eq, PartialEq)]
    pub enum ChainName {
        Edgeware,
        Webb,
        Ganache,
        Beresheet,
        Harmoney,
    }

    pub trait EvmChain {
        fn name() -> ChainName;
        fn endpoint() -> &'static str;
        fn chain_id() -> u32;
        fn contracts() -> HashMap<&'static str, u128>;
    }

    macro_rules! define_chain {
        ($name:ident => {
            endpoint: $endpoint:expr,
            chain_id: $chain_id:expr,
            contracts: [
                $({
                    size: $size:expr,
                    address: $address:expr,
                }),*
            ],
        }) => {
            #[derive(Debug, Clone, Copy, Eq, PartialEq)]
            pub struct $name;
            impl EvmChain for $name {
                fn name() -> ChainName { ChainName::$name }

                fn endpoint() -> &'static str { $endpoint }

                fn chain_id() -> u32 { $chain_id }

                fn contracts() -> HashMap<&'static str, u128> {
                    #[allow(unused_mut)]
                    let mut map = HashMap::new();
                    $(
                        map.insert($address, $size);
                    )*
                    map
                }
            }
       };
    }

    define_chain! {
        Edgeware => {
            endpoint: "https://mainnet1.edgewa.re/evm",
            chain_id: 2021,
            contracts: [],
        }
    }

    define_chain! {
        Ganache => {
            endpoint: "http://localhost:1998",
            chain_id: 1337,
            contracts: [
                {
                    size: 1,
                    address: "0xF759e19b1142079b1963e1E323B07e4AC67aB899",
                }
            ],
        }
    }

    define_chain! {
        Beresheet => {
            endpoint: "http://beresheet1.edgewa.re:9933",
            chain_id: 2022,
            contracts: [
                {
                    size: 10,
                    address: "0x5f771fc87F87DB48C9fB11aA228D833226580689",
                },
                {
                    size: 100,
                    address: "0x2ee2e51cab1561E4482cacc8Be8b46CE61E46991",
                },
                {
                    size: 1000,
                    address: "0x5696b4AfBc169454d7FA26e0a41828d445CFae20",
                },
                {
                    size: 10000,
                    address: "0x626FEc5Ffa7Bf1EE8CEd7daBdE545630473E3ABb",
                }
            ],
        }
    }

    define_chain! {
        Harmoney => {
            endpoint: "https://api.s1.b.hmny.io",
            chain_id: 1666700001,
            contracts: [
                {
                    size: 1,
                    address: "0x59DCE3dcA8f47Da895aaC4Df997d8A2E29815B1B",
                },
                {
                    size: 100,
                    address: "0xF06fA633f6E801d9fF3D450Af8806489D4fa70a1",
                }
            ],
        }
    }

    define_chain! {
        Webb => {
            endpoint: "",
            chain_id: 0,
            contracts: [],
        }
    }
}
