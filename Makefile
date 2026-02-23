SANDBOX_IMAGE := brainlog-sandbox:v1

.PHONY: sandbox sandbox-build

sandbox-build:
	docker build -t $(SANDBOX_IMAGE) -f Dockerfile.sandbox .

sandbox: sandbox-build
	docker sandbox run -t $(SANDBOX_IMAGE) claude . $(ARGS)
