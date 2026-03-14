  ⎿  Error: Exit code 1
     Argus Validation Tests
     Supervisor: /build/target/aarch64-unknown-linux-musl/debug/supervisor
     Arch: aarch64

     Test 1: Basic Process Tracing
       PASS
     Test 2: Stdio Capture
       PASS: stdout=1 stderr=1
     Test 3: File Write + Read + Delete
       FAIL: no write event with after_hash for /tmp/argus-test-workspace/test.txt
       MISSING: expected 'read' event (cat read)
     Test 4: Pipe Topology
       FAIL: no pipe_create events (got 0)
       FAIL: no pipe_data events (got 0)
     Test 5: Subprocess Tree
       FAIL: no pipe_data from ls to python (0)
     Test 6: Self-Created Tool (Escape Test)
       FAIL: no exec event for python3 running tool
       FAIL: no write event for /tmp/argus-test-workspace/tool-output.txt
     Test 7: Write Locking
       FAIL: no write events for /tmp/argus-test-workspace/shared.txt
