# Picture Cleaner

Windows向けの画像整理デスクトップアプリです。

## MVP

- 対象OSはWindowsのみ
- UIは日本語のみ
- 対象画像形式は `jpg`, `jpeg`, `png`, `webp`
- サブフォルダを含める設定は初期ON
- 完全重複、類似画像、ブレの可能性を検出
- 類似画像は自動削除せず、横並びで比較
- 選択した画像だけOSのゴミ箱へ移動
- スキャン結果はアプリ終了時に保存しない

## 開発

```powershell
npm.cmd install
npm.cmd run tauri:dev
```

このアプリはTauriを使うため、WindowsではRustとMicrosoft C++ Build Toolsが必要です。
