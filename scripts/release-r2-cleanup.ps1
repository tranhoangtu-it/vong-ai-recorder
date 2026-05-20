<#
.SYNOPSIS
    Remove dummy/test release artifacts from Cloudflare R2 and GitHub after a dry run.

.DESCRIPTION
    Deletes:
    - R2 objects under releases/<tag>/ (versioned path)
    - R2 objects under releases/latest/ (only if --IncludeLatest is passed)
    - Local git tag (local only — remote deletion is a separate command shown at end)

    Does NOT automatically delete the GitHub draft release — do that in the UI.

.PARAMETER Tag
    The git tag to clean up. Example: "v0.0.0-beta.0"

.PARAMETER AccountId
    Cloudflare Account ID (32-char hex). If omitted, reads from $env:R2_ACCOUNT_ID.

.PARAMETER AccessKeyId
    R2 API Access Key ID. If omitted, reads from $env:AWS_ACCESS_KEY_ID.

.PARAMETER SecretAccessKey
    R2 API Secret Access Key. If omitted, reads from $env:AWS_SECRET_ACCESS_KEY.

.PARAMETER BucketName
    R2 bucket name. Defaults to "vong-releases".

.PARAMETER IncludeLatest
    Also delete objects in releases/latest/ (use only if you know this tag was
    mirrored to latest/ and no real release has overwritten it yet).

.PARAMETER WhatIf
    Show what would be deleted without actually deleting anything.

.EXAMPLE
    # Dry-run cleanup — show what would be deleted
    .\scripts\release-r2-cleanup.ps1 -Tag "v0.0.0-beta.0" -WhatIf

.EXAMPLE
    # Real cleanup with credentials from environment
    $env:R2_ACCOUNT_ID      = "a1b2c3..."
    $env:AWS_ACCESS_KEY_ID  = "abc..."
    $env:AWS_SECRET_ACCESS_KEY = "xyz..."
    .\scripts\release-r2-cleanup.ps1 -Tag "v0.0.0-beta.0"

.EXAMPLE
    # Cleanup including latest/ mirror
    .\scripts\release-r2-cleanup.ps1 -Tag "v0.0.0-beta.0" -IncludeLatest
#>

[CmdletBinding(SupportsShouldProcess)]
param(
    [Parameter(Mandatory)]
    [string]$Tag,

    [string]$AccountId = $env:R2_ACCOUNT_ID,
    [string]$AccessKeyId = $env:AWS_ACCESS_KEY_ID,
    [string]$SecretAccessKey = $env:AWS_SECRET_ACCESS_KEY,
    [string]$BucketName = "vong-releases",
    [switch]$IncludeLatest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# ── Validate inputs ────────────────────────────────────────────────────────

if (-not $AccountId) {
    Write-Error "AccountId is required. Pass -AccountId or set `$env:R2_ACCOUNT_ID."
}
if (-not $AccessKeyId) {
    Write-Error "AccessKeyId is required. Pass -AccessKeyId or set `$env:AWS_ACCESS_KEY_ID."
}
if (-not $SecretAccessKey) {
    Write-Error "SecretAccessKey is required. Pass -SecretAccessKey or set `$env:AWS_SECRET_ACCESS_KEY."
}
if ($Tag -notmatch '^v\d') {
    Write-Error "Tag must start with 'v' (e.g. 'v0.0.0-beta.0'). Got: $Tag"
}

# ── Set AWS CLI environment for R2 ────────────────────────────────────────

$env:AWS_ACCESS_KEY_ID     = $AccessKeyId
$env:AWS_SECRET_ACCESS_KEY = $SecretAccessKey
$env:AWS_DEFAULT_REGION    = "auto"
$env:AWS_ENDPOINT_URL      = "https://$AccountId.r2.cloudflarestorage.com"

$s3Prefix  = "s3://$BucketName"
$versionedPath = "$s3Prefix/releases/$Tag/"

Write-Output ""
Write-Output "=== Vọng R2 Cleanup ==="
Write-Output "Tag:    $Tag"
Write-Output "Bucket: $BucketName"
Write-Output "Path:   $versionedPath"
if ($IncludeLatest) { Write-Output "Also cleaning: releases/latest/" }
Write-Output ""

# ── List objects first (so user sees what will be deleted) ────────────────

Write-Output "Objects under $($versionedPath):"
$listOutput = aws s3 ls $versionedPath 2>&1
if ($LASTEXITCODE -ne 0) {
    Write-Warning "Could not list objects (bucket may be empty or credentials wrong):`n$listOutput"
} else {
    Write-Output $listOutput
}

if ($IncludeLatest) {
    Write-Output ""
    Write-Output "Objects under $s3Prefix/releases/latest/:"
    aws s3 ls "$s3Prefix/releases/latest/" 2>&1 | Write-Output
}

Write-Output ""

# ── Delete versioned path ─────────────────────────────────────────────────

if ($PSCmdlet.ShouldProcess($versionedPath, "Delete all R2 objects recursively")) {
    Write-Output "Deleting $versionedPath ..."
    aws s3 rm $versionedPath --recursive
    if ($LASTEXITCODE -ne 0) {
        Write-Error "Failed to delete R2 objects at $versionedPath"
    }
    Write-Output "Done — versioned path deleted."
}

# ── Delete latest/ mirror (optional) ─────────────────────────────────────

if ($IncludeLatest) {
    $latestPath = "$s3Prefix/releases/latest/"
    if ($PSCmdlet.ShouldProcess($latestPath, "Delete all R2 objects recursively")) {
        Write-Output "Deleting $latestPath ..."
        aws s3 rm $latestPath --recursive
        if ($LASTEXITCODE -ne 0) {
            Write-Error "Failed to delete R2 objects at $latestPath"
        }
        Write-Output "Done — latest/ path deleted."
    }
}

# ── Local git tag cleanup ─────────────────────────────────────────────────

Write-Output ""
Write-Output "=== Git Tag Cleanup ==="

$tagExists = git tag --list $Tag
if ($tagExists) {
    if ($PSCmdlet.ShouldProcess("local git tag $Tag", "Delete")) {
        git tag -d $Tag
        Write-Output "Local tag $Tag deleted."
    }
} else {
    Write-Output "Local tag $Tag not found (already deleted or never created locally)."
}

# ── Instructions for remaining manual steps ───────────────────────────────

Write-Output ""
Write-Output "=== Remaining Manual Steps ==="
Write-Output ""
Write-Output "1. Delete remote git tag (requires push permission):"
Write-Output "      git push --delete origin $Tag"
Write-Output ""
Write-Output "2. Delete draft GitHub Release:"
Write-Output "   GitHub UI → vong-ai-recorder → Releases → $Tag → Edit → Delete this release"
Write-Output "   Or via CLI:"
Write-Output "      gh release delete $Tag --repo tranhoangtu-it/vong-ai-recorder --yes"
Write-Output ""
Write-Output "Cleanup complete."
