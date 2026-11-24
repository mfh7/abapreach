
  METHOD run_type.
    CASE mv_mode.
      WHEN `RUN_SNIPPET`.
        TRY.
            run_snippet( ).
          CATCH cx_root INTO DATA(lx_error).
            run_failed( lx_error ).
            RETURN.
        ENDTRY.
        cut_note( ).
        finish( `RAN` ).
      WHEN OTHERS.
        reject( `This runner has no template for the requested mode` ).
    ENDCASE.
  ENDMETHOD.


  METHOD print.
    " The one output of a snippet: a line of text, counted against the output limit.
    data_line( iv_text ).
  ENDMETHOD.


  METHOD run_snippet.
    " The statements of the snippet start on the next line.
    " ABAPREACH SNIPPET BODY
  ENDMETHOD.
