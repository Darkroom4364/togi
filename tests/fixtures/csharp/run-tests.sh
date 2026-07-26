#!/usr/bin/env bash
set -euo pipefail

build_dir="$(mktemp -d)"
trap 'rm -rf "$build_dir"' EXIT
export DOTNET_CLI_HOME="$build_dir/dotnet-cli"
export NUGET_PACKAGES="$build_dir/nuget"

cp Calc.cs CalcTest.cs "$build_dir"
cat > "$build_dir/Fixture.csproj" <<'EOF'
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <OutputType>Exe</OutputType>
    <TargetFramework>net8.0</TargetFramework>
    <ImplicitUsings>disable</ImplicitUsings>
    <Nullable>enable</Nullable>
  </PropertyGroup>
</Project>
EOF

dotnet restore "$build_dir/Fixture.csproj" --ignore-failed-sources --nologo
dotnet run --project "$build_dir/Fixture.csproj" --configuration Release --no-restore --nologo
