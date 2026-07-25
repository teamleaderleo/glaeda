.PHONY: work vm vm-create vm-bootstrap vm-check vm-up vm-tmux vm-status vm-sync vm-doctor vm-observe vm-stop

work:
	@bash scripts/macbook-workspace.sh

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
	@bash scripts/macbook-runner-vm.sh doctor

vm-observe:
	@bash scripts/macbook-runner-vm.sh observe

vm-stop:
	@bash scripts/macbook-runner-vm.sh stop
