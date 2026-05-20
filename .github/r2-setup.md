# R2 Bucket + GitHub Secrets Setup

One-time setup required before the first tag push triggers the release pipeline.

---

## 1. Create Cloudflare R2 Bucket

1. Go to [Cloudflare Dashboard](https://dash.cloudflare.com) → **Storage & Databases** → **R2 Object Storage** → **Create bucket**
2. Bucket name: `vong-releases`
3. Location: **Automatic** (Cloudflare picks based on traffic)
4. Click **Create bucket**

**Set spending limit (recommended):**
Cloudflare dashboard → **Billing** → **Spending Alerts** → Set a hard $0 cap on R2 so you never get a surprise bill during beta.

---

## 2. Enable Public Access via Custom Domain

### Option A — Custom domain `dl.vong.app` (recommended)

Your domain `vong.app` must be on Cloudflare DNS for this to work automatically.

1. Go to R2 → **vong-releases** → **Settings** → **Custom Domains** → **Connect Domain**
2. Enter `dl.vong.app` → **Continue**
3. If `vong.app` is managed by Cloudflare: the CNAME is created automatically + SSL provisioned within ~1 minute.
4. If `vong.app` is on another DNS provider: add manually:
   ```
   Type:  CNAME
   Name:  dl
   Value: pub-<your-bucket-id>.r2.dev
   TTL:   Auto
   ```
   Replace `<your-bucket-id>` with the value shown in R2 → vong-releases → Settings → R2.dev subdomain.

**Verify:** `curl -I https://dl.vong.app/` should return `200` or `404` (not a connection error) once DNS propagates (~2–5 min on Cloudflare, up to 48h elsewhere).

### Option B — R2.dev subdomain (fallback, no DNS changes needed)

If custom domain is not set up yet, use the auto-generated URL shown in R2 → **vong-releases** → **Settings** → **Public Access** → **R2.dev subdomain**:

```
https://pub-<random-id>.r2.dev/releases/v0.1.0-beta.1/Vong-AI-Recorder-0.1.0-beta.1-x86_64.msi
```

Set `R2_PUBLIC_BASE` secret (Step 4) to this URL as a temporary fallback.

---

## 3. Create R2 API Token

1. Cloudflare Dashboard → **R2 Object Storage** → **Manage R2 API Tokens** → **Create API Token**
2. Configure:
   - **Token name**: `vong-releases-ci`
   - **Permissions**: `Object Read & Write`
   - **Specify bucket**: ✅ Check → select `vong-releases`
   - **TTL**: No expiry (rotate manually if leaked)
3. Click **Create API Token**
4. **Save immediately** — the Secret Access Key is shown only once:
   - Access Key ID: `<copy this>`
   - Secret Access Key: `<copy this — never shown again>`
   - Endpoint URL: `https://<account-id>.r2.cloudflarestorage.com`

**Your Account ID** is visible in the Cloudflare Dashboard URL bar after logging in:
`https://dash.cloudflare.com/<account-id>/...` — it is a 32-character hex string.

---

## 4. Add GitHub Repository Secrets

Go to: **GitHub → vong-ai-recorder repo → Settings → Secrets and variables → Actions → New repository secret**

Add these 5 secrets (exact names — the workflow references them verbatim):

| Secret Name | Value | Example |
|---|---|---|
| `R2_ACCOUNT_ID` | Your 32-char Cloudflare Account ID | `a1b2c3d4e5f6...` |
| `R2_ACCESS_KEY_ID` | Access Key ID from Step 3 | `abc123...` |
| `R2_SECRET_ACCESS_KEY` | Secret Access Key from Step 3 | `xyz789...` |
| `R2_BUCKET` | `vong-releases` | `vong-releases` |
| `R2_PUBLIC_BASE` | Public base URL (no trailing slash) | `https://dl.vong.app` |

**Note:** `R2_BUCKET` is a secret (not a variable) so it doesn't appear in workflow logs. This is intentional.

---

## 5. Verify Setup (Local Test)

Before pushing a tag, verify the credentials work from your local machine:

```powershell
# Set credentials temporarily in your shell session
$env:AWS_ACCESS_KEY_ID     = "<your-access-key-id>"
$env:AWS_SECRET_ACCESS_KEY = "<your-secret-access-key>"
$env:AWS_DEFAULT_REGION    = "auto"
$env:AWS_ENDPOINT_URL      = "https://<account-id>.r2.cloudflarestorage.com"

# List bucket (should return empty or existing objects)
aws s3 ls "s3://vong-releases/"

# Test upload with a dummy file
"test" | Out-File test.txt -Encoding ascii
aws s3 cp test.txt "s3://vong-releases/test/test.txt"
aws s3 rm "s3://vong-releases/test/test.txt"
Remove-Item test.txt

# Verify custom domain (if configured)
curl -I "https://dl.vong.app/"
```

All commands should complete without `AccessDenied` or connection errors.

---

## 6. URL Structure Reference

After the pipeline runs for `v0.1.0-beta.1`, files will be at:

```
# Versioned (permanent, never overwritten)
https://dl.vong.app/releases/v0.1.0-beta.1/Vong-AI-Recorder-0.1.0-beta.1-x86_64.msi
https://dl.vong.app/releases/v0.1.0-beta.1/Vong-AI-Recorder-0.1.0-beta.1-vulkan-x86_64.msi
https://dl.vong.app/releases/v0.1.0-beta.1/SHA256SUMS

# Latest (always points to most recent release, overwritten on each release)
https://dl.vong.app/releases/latest/Vong-AI-Recorder-x86_64.msi
https://dl.vong.app/releases/latest/Vong-AI-Recorder-vulkan-x86_64.msi
https://dl.vong.app/releases/latest/SHA256SUMS
```

Use the **versioned URLs** on the landing page — they never change and the hash is always correct. The `latest/` paths are for users who want to always get the newest version without updating bookmarks.

---

## 7. Cleanup After Dry-Run Test

After running the pipeline with dummy tag `v0.0.0-beta.0`:

```powershell
$env:AWS_ACCESS_KEY_ID     = "<your-access-key-id>"
$env:AWS_SECRET_ACCESS_KEY = "<your-secret-access-key>"
$env:AWS_DEFAULT_REGION    = "auto"
$env:AWS_ENDPOINT_URL      = "https://<account-id>.r2.cloudflarestorage.com"

# Remove dummy R2 objects
aws s3 rm "s3://vong-releases/releases/v0.0.0-beta.0/" --recursive

# Remove dummy git tag
git tag -d v0.0.0-beta.0
git push --delete origin v0.0.0-beta.0

# Delete draft GitHub Release — do this in the GitHub UI:
# Releases → v0.0.0-beta.0 → Edit → Delete this release
```
