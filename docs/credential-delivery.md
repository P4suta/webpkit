---
doppler_automation_app: release_plz
---

# Credential delivery

The release App credentials use Doppler's [native GitHub synchronization](https://docs.doppler.com/docs/github-actions).
The [delivery decision](adr/sync-release-app-credentials-to-the-protected-environment.md) governs the cutover.
Canonical values remain in Doppler, and a purpose-specific config references the automation fields.
The [manifest](../automation/release-app-sync.toml) identifies the repository, environment, and field mapping.

The release environment admits main jobs and prevents administrator bypass.
RELEASE_APP_CLIENT_ID references GITHUB_APP_CLIENT_ID, and RELEASE_APP_PRIVATE_KEY references GITHUB_APP_PRIVATE_KEY.
The workflow passes each named secret only to the App token action after tool setup.
Each token targets the current repository and requests the consumer's permissions.
The App private key retains authority across every installation of the shared App.
GitHub stores copies, and protected main and environment policies replace Doppler workflow and event claims.

The manual **Verify credential access** workflow checks required fields and creates a repository token with contents read access only.
The action revokes the token during cleanup.
Run the workflow from reviewed main after merging.
Keep the existing Doppler identity until replacement authentication succeeds.
Retire the identity only after confirming that every consumer has migrated.

Rotate canonical values in Doppler and verify synchronization and authentication before retiring the previous key.
Keep values and authentication state out of the repository and logs.
Publication keeps its exact-source qualification, registry authentication, and release environment.
Pause the release-plz workflow during migration merge and authentication verification, then restore its previous enabled state.
Credential migration creates no release or tag.
