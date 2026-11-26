"""run_sel.py FILE SUBSTR...: run the tests of FILE whose names contain any SUBSTR (scratch, not committed)."""
import importlib.util, sys, unittest
s = importlib.util.spec_from_file_location("t", sys.argv[1]); m = importlib.util.module_from_spec(s); sys.modules["t"] = m
s.loader.exec_module(m)
ld, suite = unittest.defaultTestLoader, unittest.TestSuite()
for c in dir(m):
    k = getattr(m, c)
    if isinstance(k, type) and issubclass(k, unittest.TestCase):
        for t in ld.getTestCaseNames(k):
            if any(x in t for x in sys.argv[2:]):
                suite.addTest(k(t))
r = unittest.TextTestRunner(verbosity=1).run(suite)
sys.exit(not r.wasSuccessful())
