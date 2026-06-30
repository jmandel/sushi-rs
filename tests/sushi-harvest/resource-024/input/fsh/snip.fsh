
        Resource: TestResource
        * isValid 1..1 MS boolean "is it valid?"
        * stuff 0..* string "just stuff" "a list of some stuff"
        * address 1..* Address "Just an address" 
          """
            This definition for address includes markdown for unordered lists:
            * Level 1 list item 1
              * Level 2 list item 1a
              * Level 2 list item 1b
            * Level 1 list item 2
          """
        