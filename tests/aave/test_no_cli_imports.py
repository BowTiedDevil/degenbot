def test_aave_package_has_no_cli_imports():
    """Verify that degenbot.aave never imports from degenbot.cli."""
    import subprocess

    result = subprocess.run(
        ["grep", "-rn", "from degenbot.cli", "src/degenbot/aave/"],
        capture_output=True,
    )
    assert result.returncode != 0, (
        f"aave/ leaks CLI dependency:\n{result.stdout.decode()}"
    )
