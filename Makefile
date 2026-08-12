.PHONY: work work-cmux work-cmux-setup work-tmux vm vm-create vm-bootstrap vm-check vm-up vm-tmux vm-status vm-sync vm-doctor vm-observe vm-stop

work:
	@bash scripts/macbook-workspace.sh auto

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
	@bash scripts/macbook-workspace.sh sync-cmux || true

vm-tmux:
	@bash scripts/macbook-runner-vm.sh tmux

vm-status:
	@bash scripts/macbook-runner-vm.sh status
	@bash scripts/macbook-workspace.sh sync-cmux || true

vm-sync:
	@bash scripts/macbook-runner-vm.sh sync
	@bash scripts/macbook-workspace.sh sync-cmux || true

vm-doctor:
	@status=0; bash scripts/macbook-runner-vm.sh doctor || status=$$?; \
	  bash scripts/macbook-workspace.sh notify-doctor "$$status" || true; \
	  bash scripts/macbook-workspace.sh sync-cmux || true; \
	  exit "$$status"

vm-observe:
	@bash scripts/macbook-runner-vm.sh observe

vm-stop:
	@bash scripts/macbook-runner-vm.sh stop
	@bash scripts/macbook-workspace.sh sync-cmux || true
