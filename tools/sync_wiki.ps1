# GitHub Wiki 同期スクリプト
$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot
$wikiDir = Join-Path $repoRoot "wiki"
$tempWiki = Join-Path $env:TEMP "GameAssistant.wiki"

Write-Host "🔄 GitHub Wiki リポジトリをクローンしています..." -ForegroundColor Cyan
if (Test-Path $tempWiki) {
    Remove-Item -Recurse -Force $tempWiki
}

try {
    git clone https://github.com/k0ta0uchi/GameAssistant.wiki.git $tempWiki
} catch {
    Write-Host "⚠️ GitHub 上で Wiki がまだ作成されていません。" -ForegroundColor Yellow
    Write-Host "👉 ブラウザで https://github.com/k0ta0uchi/GameAssistant/wiki を開き、「Create the first page」をクリックして最初のページを作成してください。" -ForegroundColor Green
    exit 1
}

Write-Host "📋 Wiki ファイルをコピーしています..." -ForegroundColor Cyan
Copy-Item -Path "$wikiDir\*" -Destination $tempWiki -Recurse -Force

Set-Location $tempWiki
git add .
$status = git status --porcelain
if ($status) {
    git commit -m "docs: sync GitHub wiki from repository wiki/"
    git push origin master
    Write-Host "✅ GitHub Wiki の同期が完了しました！" -ForegroundColor Green
} else {
    Write-Host "ℹ️ 変更はありません。Wiki は最新です。" -ForegroundColor Gray
}
