// CallXL additions to webrtc-sys (MIT, CallXL authors). See CALLXL_CHANGES.md.

#[cxx::bridge(namespace = "livekit_ffi")]
pub mod ffi {
    #[derive(Clone, Debug, Default)]
    pub struct CallXlCandidateInfo {
        pub address: String,
        pub port: u16,
        /// 4 or 6
        pub family: i32,
        /// host | srflx | prflx | relay
        pub typ: String,
        pub protocol: String,
        pub priority: u32,
        pub network_name: String,
        pub foundation: String,
    }

    extern "C++" {
        include!("livekit/peer_connection_factory.h");
        type PeerConnectionFactory = crate::peer_connection_factory::ffi::PeerConnectionFactory;
        type CallXlFactoryOptions = crate::peer_connection_factory::ffi::CallXlFactoryOptions;
        type Candidate = crate::candidate::ffi::Candidate;
    }

    unsafe extern "C++" {
        include!("livekit/callxl.h");

        fn create_callxl_peer_connection_factory(options: CallXlFactoryOptions) -> SharedPtr<PeerConnectionFactory>;

        fn callxl_candidate_info(candidate: &Candidate) -> CallXlCandidateInfo;
    }
}
