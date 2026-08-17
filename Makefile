.PHONY: work work-cmux work-cmux-setup work-tmux vm vm-create vm-bootstrap vm-check vm-up vm-tmux vm-status vm-sync vm-doctor vm-observe vm-stop quarry-runner-install quarry-runner-status quarry-runner-route quarry-runner-unroute quarry-runner-pause quarry-runner-resume quarry-runner-remove

work:
	@bash scripts/macbook-workspace.sh tmux

work-cmux:
	@bash scripts/macbook-workspace.sh cmux

work-cmux-setup:
	@bash scripts/macbook-workspace.sh setup-cmux

work-tmux:
	@bash scripts/macbook-workspace.sh tmux

vm:
	@bash scripts/macbook-runner-vm.sh shell

vm-create:
	@bash scripts/macbook-runner-bootstrap.sh create

vm-bootstrap:
	@bash scripts/macbook-runner-bootstrap.sh bootstrap

vm-check:
	@bash scripts/macbook-runner-bootstrap.sh check

vm-up:
	@bash scripts/macbook-runner-vm.sh up

vm-tmux:
	@bash scripts/macbook-runner-vm.sh tmux

vm-status:
	@bash scripts/macbook-runner-vm.sh status

vm-sync:
	@bash scripts/macbook-runner-vm.sh sync

vm-doctor:
	@status=0; bash scripts/macbook-runner-vm.sh doctor || status=$$?; \
	  bash scripts/macbook-workspace.sh notify-doctor "$$status" || true; \
	  exit "$$status"

vm-observe:
	@bash scripts/macbook-runner-vm.sh observe

vm-stop:
	@bash scripts/macbook-runner-vm.sh stop

quarry-runner-install:
	@bash scripts/quarry-trusted-runner.sh install

quarry-runner-status:
	@bash scripts/quarry-trusted-runner.sh status

quarry-runner-route:
	@bash scripts/quarry-trusted-runner.sh route

quarry-runner-unroute:
	@bash scripts/quarry-trusted-runner.sh unroute

quarry-runner-pause:
	@bash scripts/quarry-trusted-runner.sh pause

quarry-runner-resume:
	@bash scripts/quarry-trusted-runner.sh resume

quarry-runner-remove:
	@bash scripts/quarry-trusted-runner.sh remove
