pub use webrtc_sys;

#[cfg(test)]
mod tests {
    #[test]
    fn factory_links_and_creates() {
        let f = webrtc_sys::peer_connection_factory::ffi::create_peer_connection_factory();
        assert!(!f.is_null());
    }
}
