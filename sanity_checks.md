# Arc overuse — find every Arc usage
docker exec argus-arm64 grep -rn "Arc<" crates/argus/src --include="*.rs" | wc -l
docker exec argus-arm64 grep -rn "Arc<" crates/argus/src --include="*.rs" | grep -v "test" | sed 's/:.*//' | sort | uniq -c | sort -rn | head -20

# Arc<Mutex<>> — the pattern we specifically said to eliminate
docker exec argus-arm64 grep -rn "Arc<Mutex" crates/argus/src --include="*.rs"
docker exec argus-arm64 grep -rn "Arc<RwLock" crates/argus/src --include="*.rs"

# .clone() abuse — excessive cloning often hides ownership issues
docker exec argus-arm64 grep -rn "\.clone()" crates/argus/src --include="*.rs" | wc -l
docker exec argus-arm64 grep -rn "\.clone()" crates/argus/src --include="*.rs" | sed 's/:.*//' | sort | uniq -c | sort -rn | head -20

# .unwrap() — panics in production code
docker exec argus-arm64 grep -rn "\.unwrap()" crates/argus/src --include="*.rs" | grep -v test | grep -v "mod tests" | wc -l
docker exec argus-arm64 grep -rn "\.unwrap()" crates/argus/src --include="*.rs" | grep -v test

# .expect() without good messages
docker exec argus-arm64 grep -rn '\.expect("' crates/argus/src --include="*.rs" | grep -v test

# String where &str would do — functions taking String instead of &str or impl AsRef<str>
docker exec argus-arm64 grep -rn "fn.*String)" crates/argus/src --include="*.rs" | grep -v "-> String" | head -20

# Box<dyn> where generics would work (per your Rust guidelines: prefer concrete > generic > dyn)
docker exec argus-arm64 grep -rn "Box<dyn" crates/argus/src --include="*.rs" | grep -v test | wc -l

# Mutex in async context — should be tokio::sync::Mutex if held across .await
docker exec argus-arm64 grep -rn "std::sync::Mutex" crates/argus/src --include="*.rs" | grep -v test

# to_string() / format!() in hot paths — check tracer and pipeline
docker exec argus-arm64 grep -rn "to_string()\|format!" crates/argus/src/pipeline --include="*.rs" | wc -l

# Vec<u8> allocations in capture path
docker exec argus-arm64 grep -rn "Vec::new()\|vec!\[" crates/argus/src/pipeline/stages/capture.rs

# Clippy catches most of the rest
docker exec argus-arm64 cargo clippy --workspace -- \
    -W clippy::redundant_clone \
    -W clippy::needless_pass_by_value \
    -W clippy::large_enum_variant \
    -W clippy::rc_buffer \
    -W clippy::mutex_atomic \
    -W clippy::needless_collect \
    -W clippy::inefficient_to_string \
    2>&1 | head -50
