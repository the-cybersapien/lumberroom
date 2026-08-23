"""
Regression test for the single-owner warning in openwebui-filter.py.

httpx and pydantic (the filter's two runtime dependencies, both supplied by OpenWebUI's own
backend) are not guaranteed to be installed wherever this repo is checked out, so this test reads
the file as text rather than importing the Filter class. What it guards is documentation staying
in place, not runtime behaviour: the filter has exactly one token valve shared by every account it
runs for, and the warning is the only thing standing between an operator and enabling it on a
multi-account instance.

Run: python3 client/openwebui-filter_test.py
"""

import pathlib
import unittest

SOURCE = (pathlib.Path(__file__).parent / "openwebui-filter.py").read_text()


class SingleOwnerWarningStaysInThePlace(unittest.TestCase):
    def test_the_module_docstring_names_the_single_owner_restriction(self):
        self.assertIn("SINGLE-OWNER ONLY", SOURCE)
        self.assertIn("DO NOT ENABLE THIS ON A MULTI-ACCOUNT OPENWEBUI INSTANCE", SOURCE)

    def test_the_module_docstring_explains_why_the_cache_key_is_not_an_access_boundary(self):
        self.assertIn("cache-locality choice, not an access boundary", SOURCE)

    def test_the_token_valve_description_points_back_at_the_warning(self):
        self.assertIn("single-owner OpenWebUI deployments only", SOURCE)


if __name__ == "__main__":
    unittest.main()
