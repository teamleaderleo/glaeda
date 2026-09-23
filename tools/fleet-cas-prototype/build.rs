fn main() {
    let fds = protox::compile(
        [
            "compilation_caching_cas.proto",
            "compilation_caching_kv.proto",
        ],
        ["proto"],
    )
    .unwrap();
    tonic_prost_build::configure()
        .build_client(false)
        .compile_fds(fds)
        .unwrap();
}
