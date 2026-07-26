# Agent coordination protocol

This document defines how multiple implementation agents coordinate work on SmolRunner. The goal is to keep delegation observable, bounded, and recoverable. An agent must never disappear into an indefinite scheduled wait while the coordinator assumes progress is still happening.

## Core rule

Repository work is performed in the active work session and ends with an observable repository artifact or an explicit blocked result.

Scheduled tasks, reminders, recurring checks, and conditional watches are not an agent-to-agent signalling mechanism. Do not create them to wake another agent, poll another agent, wait for a review, or resume delegated implementation. Scheduling is reserved for a future user-facing reminder or monitoring request that the human operator explicitly asked for.

## Required delegation contract

Before delegating implementation, record all of the following:

- exact repository and base commit SHA;
- issue or objective;
- files or subsystem owned by the agent;
- files and authority explicitly outside its scope;
- concrete deliverable, normally a branch, commit, pull request, review, or issue comment;
- required checks;
- completion signal;
- bounded recovery rule when the signal does not appear.

A useful completion signal is externally observable and exact, for example:

- branch name plus head commit SHA;
- pull request number plus exact tested head SHA;
- review or issue comment that names the exact commit inspected;
- workflow run IDs and conclusions for that exact commit.

A private chat state, an agent saying it is waiting, or an unspecified future event is not a completion signal.

## No passive waiting

An implementation agent has only three valid terminal states:

1. **Completed:** publish the requested artifact and report its exact identity.
2. **Blocked:** report the exact missing dependency, current branch or commit, partial work, and the action required to unblock it.
3. **Failed:** report the exact failure and leave the repository in a reviewable or clean state.

`Waiting`, `scheduled`, `listening`, and `monitoring` are not terminal states for repository implementation.

The coordinator must continue independent work rather than pause for delegated agents. It may check the declared completion signal at most twice during the active coordination pass. When the artifact is still absent after the second check, classify the delegation as stalled and immediately take over, reassign, or reduce the scope. Do not create a scheduled task as a third check.

## Dependency signalling

An agent may depend on another branch or pull request only when the dependency and signal are explicit. Use repository-visible signals:

- merged commit appears on `main`;
- named pull request reaches an exact reviewed head;
- named workflow succeeds on that exact head;
- named issue or review comment is posted.

The waiting agent must not remain open-ended. It should complete all independent work, report the dependency, and stop. The coordinator resumes or reassigns the dependent work after observing the signal.

## Recovery from a stalled agent

When an agent becomes unresponsive or leaves scheduled work behind:

1. Inspect its branch, pull request, comments, and workflow runs.
2. Record the last exact observable head and whether any partial work is usable.
3. Ignore or cancel scheduled coordination tasks; they carry no repository authority.
4. Refresh `main` and every relevant branch before continuing.
5. Take ownership or reassign from the last safe commit.
6. Avoid duplicate mutations and never assume an unseen agent action completed.
7. Run the normal checks and review the complete final diff before declaring recovery complete.

## Scheduling policy

A scheduled task is acceptable only when all of these are true:

- the human operator explicitly requested future or recurring delivery;
- the task observes an external condition or delivers a user-facing reminder;
- its cadence and timezone are explicit;
- its notification condition is explicit;
- its stop condition or continued value is clear;
- repository progress does not depend on another agent receiving it.

Never schedule a task merely because an agent cannot continue in the current response. Never use a scheduled task to substitute for a direct status report, explicit block, branch handoff, or immediate best-effort completion.

## Handoff template

Use this compact handoff for delegated repository work:

```text
Repository: owner/repo
Base: <exact SHA>
Objective: <one concrete outcome>
Owned scope: <files/subsystem>
Excluded scope: <authority and files>
Deliverable: <branch/PR/review/comment>
Checks: <exact commands or workflows>
Completion signal: <observable artifact and exact identity>
Recovery: after two missing-signal checks, coordinator takes over or reassigns
```

## Anti-patterns

Do not:

- tell another agent to wait indefinitely for an unspecified signal;
- create reminders or condition watches to coordinate implementation agents;
- leave an agent in a listening state after its independent work is complete;
- rely on hidden chat state as proof of completion;
- keep polling without a fixed limit;
- block the coordinator when independent work remains;
- assume a scheduled task was received, executed, or understood;
- treat silence as success.

The repository is the coordination surface. Exact commits, pull requests, comments, and workflow conclusions are the signals.