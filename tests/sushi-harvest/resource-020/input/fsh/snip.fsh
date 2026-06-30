
        Resource: TestResource
        * status ^short = "foo"
        * status ^definition = """
          This definition includes markdown for unordered lists:
          * Level 1 list item 1
            * Level 2 list item 1a
            * Level 2 list item 1b
          * Level 1 list item 2
        """
        * status ^sliceIsConstraining = false
        * status ^code[0] = foo#bar "baz"
        