import unittest
from compare import label, summarize


class ScoringTests(unittest.TestCase):
    def test_invalid_text_is_not_extracted_or_converted_to_abstention(self):
        for value in ('Answer: A', 'ABC', 'C because uncertain', 'fn main() {}', '', None):
            self.assertIsNone(label(value))
        self.assertEqual(label(' C\n'), 'C')

    def test_invalid_on_abstention_case_is_not_correct(self):
        rows=[{'id':'x','prediction':None,'expected':'C','request_ms':1},
              {'id':'y','prediction':'B','expected':'A','request_ms':2},
              {'id':'z','prediction':'C','expected':'C','request_ms':3}]
        result=summarize(rows)
        self.assertEqual(result['correct'],1)
        self.assertEqual(result['invalid'],1)
        self.assertEqual(result['explicit_abstentions'],1)
        self.assertEqual(result['missed_review_as_unrelated'],1)


if __name__=='__main__':
    unittest.main()
