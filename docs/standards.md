# Standards adopted for Speakeasy

Speakeasy adapts the shared and C# profiles from
[KrishRVH/standards](https://github.com/KrishRVH/standards/tree/dc0aca28276df60b85583af9db32d30be0cfc5c0),
reviewed at revision `dc0aca28276df60b85583af9db32d30be0cfc5c0` on 2026-09-09.
This is a scoped copy, with no runtime or build dependency on that repository.
The catalog's root agent guide and configuration were not used as project
defaults.

## Kept and adapted

| Source | Speakeasy decision |
| --- | --- |
| `shared/AGENTS.md` | Keep explicit ownership, simple code, targeted reads, useful regression tests, clear prose, and Conventional Commits. Replace the generic layout and command list with this WinForms app's boundaries and native .NET commands. |
| `shared/CLAUDE.md` | Keep the single import of `AGENTS.md` so guidance has one owner. |
| `shared/.gitattributes` | Keep normalized text and binary classification. Retain only C#, project/config files, Markdown, Windows scripts, and assets used here. |
| `shared/.gitignore` | Keep local environment, IDE/build output, model/tool downloads, and recordings ignored. Retain examples and NuGet lockfiles in source control. |
| `C#/.editorconfig` | Keep UTF-8, whitespace, indentation, and familiar C# formatting. Use suggestions for optional style choices; avoid adding hundreds of naming or style gates. |
| `C#/global.json` | Pin the installed and verified .NET SDK `10.0.100`, with prerelease and roll-forward disabled. Keep the existing xUnit/VSTest runner. |
| `C#/Directory.Build.props` | Keep nullable checking, compiler warnings as errors, deterministic output, the SDK's stable .NET 10 analyzers, and package lockfiles. Preserve SDK implicit usings and normal WinForms compilation. |
| `C#/Directory.Packages.props` | Centralize the app's existing NuGet versions. Disable project-local version overrides and retain transitive resolution in generated locks. |
| `C#/AGENTS.md` and `C#/APPLICATION.md` | Keep explicit dependencies, resource ownership, cancellation, and tests at real boundaries. Map those rules to the controller and Windows adapters. |

The most important boundaries for this app are global hotkey edges, the hard
recording limit, microphone lifetime, cancellation, delayed provider callbacks,
and clipboard ownership. The test suite focuses on those contracts and uses
fakes for side effects during automated runs.

## Deliberately omitted

This personal workstation app uses the existing .NET and PowerShell tools.
It does not adopt mise, Dagger, a container workflow, a JavaScript toolchain,
MSTest/Microsoft Testing Platform migration, third-party analyzer bundles,
banned-API scanners, mutation/fuzz tooling, coverage floors, SBOM generation,
dedicated secret-scanning infrastructure, or hosted review/branch-protection
requirements. These add maintenance and setup without resolving a current
requirement. Normal NuGet restore integrity and ignored local secrets remain.

ASP.NET, Razor, EF Core, databases, authentication, subscriptions, and
monetization policies do not apply. Desktop code needs Win32 interop, an STA
thread, a synchronization context, native handles, and event callbacks; broad
application bans on those mechanisms would conflict with the product.

No global checked-arithmetic setting, public-API documentation gate, or new
style analyzer is introduced. Existing numeric validation and regression tests
cover the relevant audio/configuration boundaries; native bit patterns and
platform types remain explicit in the adapters.

## Verification

`AGENTS.md` lists the repeatable restore, Release build, test, and whitespace
checks. `Directory.Build.props` enables locked restore when `CI=true`; local
verification uses `dotnet restore Speakeasy.sln --locked-mode` explicitly.
Package or SDK upgrades are deliberate edits followed by regenerated lockfiles
and the same checks. Additional tooling can be adopted later for a concrete
failure mode without importing the rest of the catalog.
