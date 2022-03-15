TEMP_TEST_OUTPUT=/tmp/contract-test-service.log

build-contract-tests:
	@cargo build

start-contract-test-service: build-contract-tests
	@./target/debug/sse-test-api

start-contract-test-service-bg:
	@echo "Test service output will be captured in $(TEMP_TEST_OUTPUT)"
	@make start-contract-test-service >$(TEMP_TEST_OUTPUT) 2>&1 &

run-contract-tests:
	@curl -s https://raw.githubusercontent.com/launchdarkly/sse-contract-tests/master/downloader/run.sh \
      | VERSION=v2 PARAMS="-url http://localhost:8080 -debug -stop-service-at-end $(SKIPFLAGS) $(TEST_HARNESS_PARAMS)" sh

contract-tests: build-contract-tests start-contract-test-service-bg run-contract-tests

.PHONY: build-contract-tests start-contract-test-service run-contract-tests contract-tests
