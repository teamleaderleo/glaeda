.PHONY: vm vm-up vm-tmux vm-status vm-sync vm-doctor vm-observe vm-stop

vm:
	@bash scripts/macbook-runner-vm.sh shell

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
