/**
 * @file Groovy grammar for tree-sitter
 * @author Amaan Qureshi <amaanq12@gmail.com>
 * @license MIT
 */

/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

const Java = require('tree-sitter-java/grammar.js');

/* eslint-disable no-multi-spaces */

const PREC = {
  // https://introcs.cs.princeton.edu/java/11precedence/
  COMMENT: 0,         // //  /*  */
  ASSIGN: 1,          // =  += -=  *=  /=  %=  &=  ^=  |=  <<=  >>=  >>>=
  DECL: 2,
  ELEMENT_VAL: 2,
  TERNARY: 3,         // ?:
  OR: 4,              // ||
  AND: 5,             // &&
  BIT_OR: 6,          // |
  BIT_XOR: 7,         // ^
  BIT_AND: 8,         // &
  EQUALITY: 9,        // ==  !=
  GENERIC: 10,
  REL: 10,            // <  <=  >  >=  instanceof
  SHIFT: 11,          // <<  >>  >>>
  ADD: 12,            // +  -
  MULT: 13,           // *  /  %
  CAST: 14,           // (Type)
  OBJ_INST: 14,       // new
  RANGE: 15,          // ..
  UNARY: 16,          // ++a  --a  a++  a--  +  -  !  ~
  POWER: 17,
  ARRAY: 18,          // [Index]
  OBJ_ACCESS: 18,     // .
  PARENS: 18,         // (Expression)
  CLASS_LITERAL: 19,  // .
};

/* eslint-enable no-multi-spaces */

const DIGITS = token(choice('0', seq(/[1-9]/, optional(seq(optional('_'), sep1(/[0-9]+/, /_+/))))));
const DECIMAL_DIGITS = token(sep1(/[0-9]+/, '_'));

const DELIMITER = choice(';', /\n/, '\0');

module.exports = grammar(Java, {
  name: 'groovy',

  conflicts: ($, original) => original.concat([
    [$.modifiers, $.annotated_type, $.module_declaration, $.package_declaration, $.function_definition],
    [$._method_declarator, $.function_definition],

    [$.primary_expression, $._variable_declarator_id, $._unannotated_type],
    [$.primary_expression, $._variable_declarator_id],
    [$._variable_declarator_id, $._unannotated_type],
    [$.modifiers],
    [$.modifiers, $.annotated_type],

    [$.primary_expression, $.method_invocation],
    [$.field_access, $.method_invocation],
    [$.array_access, $.expression],
    [$.record_pattern, $._unannotated_type],
    [$.instanceof_expression],
    [$._type, $.array_type],
    [$.annotated_type, $.array_type],

    [$.primary_expression, $._juxt_function_name],
    [$.primary_expression, $.method_invocation, $._juxt_function_name],
    [$.primary_expression, $._variable_declarator_id, $._unannotated_type, $._juxt_function_name],
    [$.primary_expression, $._unannotated_type, $._juxt_function_name],
    [$._literal, $._juxt_function_name],
    [$.array_access, $._juxt_function_name],

    [$.lambda_expression, $._variable_declarator_id],
    [$.variable_declarator, $.bare_closure_parameter],
    [$._toplevel_statement, $.statement],
    [$.expression, $._juxt_function_name],
    [$.expression, $.array_access, $._juxt_function_name],
    [$.primary_expression, $.explicit_constructor_invocation],
    [$.primary_expression, $.method_invocation, $.record_pattern],
    [$.inferred_parameters, $.primary_expression, $._variable_declarator_id, $._unannotated_type],
    [$.inferred_parameters, $._variable_declarator_id],
    [$.inferred_parameters, $.primary_expression, $._variable_declarator_id],
  ]),

  extras: $ => [
    /\s/,
    $.line_comment,
    $.block_comment,
  ],

  rules: {
    program: $ => seq(
      optional($.shebang),
      repeat($._toplevel_statement),
    ),

    _toplevel_statement: ($, original) => choice(
      original,
      $.function_definition,
      $.juxt_function_call,
    ),

    // Java's `statement` (used inside plain `block`s: method bodies,
    // if/while/for/try/catch bodies, etc.) never gained the paren-less
    // juxt_function_call alternative that _toplevel_statement and closure
    // already have, so `println "x"` (very common) only worked at the top
    // level or inside a Groovy closure, not inside an ordinary block.
    statement: ($, original) => choice(
      original,
      $.juxt_function_call,
    ),

    declaration: ($, original) => choice(
      original,
      $.package_declaration,
    ),

    shebang: $ => token(seq('#!', /.*/)),

    // Groovy Closures are callable objects; `expr(...)` is sugar for
    // `expr.call(...)`, so any call result that is itself a Closure (e.g.
    // `Closure.rehydrate(...)`) can be invoked again immediately:
    // `foo.bar(args)(moreArgs)`. Java has no such "call the
    // result of a call" syntax, so primary_expression never gained a
    // trailing-argument-list alternative.
    primary_expression: ($, original) => choice(
      original,
      $.closure_invocation,
    ),

    closure_invocation: $ => prec.left(2, seq(
      field('function', $.primary_expression),
      field('arguments', $.argument_list),
    )),

    expression_statement: $ => seq($.expression, DELIMITER),

    expression: ($, original) => choice(
      original,
      $.closure,
      $.range_expression,
    ),

    lambda_expression: $ => seq(
      optional(
        field('parameters', choice(
          $.identifier, $.formal_parameters, $.inferred_parameters, $._reserved_identifier,
        )),
      ),
      '->',
      field('body', choice($.expression, $.block)),
    ),

    local_variable_declaration: $ => seq(
      optional($.modifiers),
      optional(choice(field('type', $._unannotated_type), 'def')),
      $._variable_declarator_list,
      DELIMITER,
    ),

    import_declaration: $ => seq(
      'import',
      optional('static'),
      $._name,
      optional(seq('.', $.asterisk)),
      optional(seq('as', $.identifier)),
      DELIMITER,
    ),

    package_declaration: $ => seq(
      repeat($._annotation),
      'package',
      $._name,
      DELIMITER,
    ),

    enhanced_for_statement: $ => seq(
      'for',
      '(',
      optional($.modifiers),
      optional(field('type', $._unannotated_type)),
      $._variable_declarator_id,
      choice(':', 'in'),
      field('value', $.expression),
      ')',
      field('body', $.statement),
    ),

    do_statement: $ => seq(
      'do',
      field('body', $.statement),
      'while',
      field('condition', $.parenthesized_expression),
      DELIMITER,
    ),

    if_statement: $ => prec.right(seq(
      'if',
      field('condition', $.parenthesized_expression),
      field('consequence', choice($.statement, $.expression)),
      optional(seq('else', field('alternative', $.statement))),
    )),

    break_statement: $ => seq('break', optional($.identifier), DELIMITER),

    continue_statement: $ => seq('continue', optional($.identifier), DELIMITER),

    return_statement: $ => seq(
      'return',
      optional($.expression),
      DELIMITER,
    ),

    yield_statement: $ => seq(
      'yield',
      $.expression,
      DELIMITER,
    ),

    catch_formal_parameter: $ => seq(
      optional($.modifiers),
      optional($.catch_type),
      $._variable_declarator_id,
    ),

    binary_expression: ($, original) => choice(
      original,
      ...[
        ['in', PREC.REL],
      ].map(([operator, precedence]) =>
        prec.left(precedence, seq(
          field('left', $.expression),
          // @ts-ignore
          field('operator', operator),
          field('right', $.expression),
        ))),
      prec.right(PREC.POWER, seq(
        field('left', $.expression),
        // @ts-ignore
        field('operator', '**'),
        field('right', $.expression),
      )),
    ),

    method_declaration: $ => seq(
      optional($.modifiers),
      $._method_header,
      choice(field('body', $.block), ';', '\n', '\0'),
    ),

    function_definition: $ => seq(
      repeat($._annotation),
      optional($.modifiers),
      choice(field('type', $._unannotated_type), 'def'),
      field('name', $.identifier),
      field('parameters', $.formal_parameters),
      field('body', $.closure),
    ),

    method_invocation: $ => prec.right(seq(
      choice(
        field('name', choice($.identifier, $._reserved_identifier)),
        seq(
          field('object', choice($.primary_expression, $.super)),
          '.',
          optional(seq(
            $.super,
            '.',
          )),
          field('type_arguments', optional($.type_arguments)),
          field('name', choice($.identifier, $._reserved_identifier)),
        ),
      ),
      choice(
        seq(
          optional(field('arguments', $.argument_list)),
          field('body', $.closure),
        ),
        seq(
          field('arguments', $.argument_list),
          optional(field('body', $.closure)),
        ),
      ),
    )),

    // Groovy `def` functions/methods commonly take untyped parameters
    // (dynamic typing), e.g. `def foo(eclipse, project) { ... }` - unlike
    // Java, where every formal_parameter requires an explicit type.
    formal_parameter: $ => seq(
      optional($.modifiers),
      optional(field('type', $._unannotated_type)),
      $._variable_declarator_id,
      optional(seq('=', $.expression)),
    ),

    argument_list: $ => seq(
      '(',
      commaSep(choice($.expression, $.map_item)),
      optional(','),
      ')',
    ),

    // Gradle's `task xyz(type: Foo) { ... }` shorthand: `xyz(type: Foo)` is
    // a normal parenthesized call, so a juxt (paren-less) argument can
    // itself be such a call (previously only literals/map_items/bare
    // identifiers were allowed, so anything after the nested call's closing
    // paren was left dangling). The trailing closure attaches to the OUTER
    // juxt call (e.g. `task testJar(type: Jar) { ... }`), mirroring
    // method_invocation's own optional trailing closure body.
    juxt_function_call: $ => prec.left(1, seq(
      field('name', $._juxt_function_name),
      field('args', alias($._juxt_argument_list, $.argument_list)),
      optional(field('body', $.closure)),
    )),

    _juxt_function_name: $ => choice(
      $.identifier,
      $.primary_expression,
      $.string_literal,
    ),

    _juxt_argument_list: $ => commaSep1(choice($._literal, $.map_item, $.identifier, $.juxt_nested_call)),

    // Narrow, purpose-built shape for `task xyz(type: Foo)`: a bare
    // identifier immediately followed by its own parenthesized argument
    // list. Deliberately NOT the general `$.method_invocation` (which
    // drags in generics/super/dot-chains and a wide new conflict surface
    // irrelevant here).
    juxt_nested_call: $ => prec.left(2, seq(
      field('name', $.identifier),
      field('arguments', $.argument_list),
    )),

    map_item: $ => seq(
      field('key', choice(
        $.identifier,
        $._literal,
        seq('(', $.expression, ')'),
      )),
      ':',
      field('value', $.expression),
    ),

    // Groovy closures with explicit parameters put the `params ->` prefix
    // FIRST, immediately after `{`, followed by zero or more statements that
    // share the SAME closing brace (e.g. `{ a, b -> stmt1; stmt2 }`). The
    // upstream `closure` rule only modeled a single trailing
    // lambda_expression/expression *after* a repeat of statements, which
    // can't represent a leading arrow followed by multiple statements. Add
    // that shape as a new alternative; the original shape (no leading
    // params, at most one trailing lambda/expression) is left untouched.
    closure: $ => prec(1, choice(
      seq(
        '{',
        repeat($.statement),
        optional(choice($.lambda_expression, $.expression)),
        '}',
      ),
      seq(
        '{',
        field('parameters', $.bare_closure_parameters),
        '->',
        repeat($.statement),
        optional($.expression),
        '}',
      ),
    )),

    bare_closure_parameter: $ => seq(
      optional(field('type', $._unannotated_type)),
      $._variable_declarator_id,
      optional(seq('=', $.expression)),
    ),

    bare_closure_parameters: $ => commaSep1($.bare_closure_parameter),

    range_expression: $ => prec.right(PREC.RANGE, seq(
      field('start', $.expression),
      '..',
      field('end', $.expression),
    )),

    array_literal: $ => prec(1, seq(
      '[',
      commaSep($.expression),
      optional(','),
      ']',
    )),

    map_literal: $ => seq(
      '[',
      commaSep($.map_item),
      optional(','),
      ']',
    ),

    _multiline_string_literal: $ => choice(
      seq(
        '\'\'\'',
        repeat(choice(
          alias($._multiline_string_fragment_single, $.multiline_string_fragment),
          $._escape_sequence,
          $.string_interpolation,
        )),
        '\'\'\'',
      ),
      seq(
        '"""',
        repeat(choice(
          alias($._multiline_string_fragment_double, $.multiline_string_fragment),
          $._escape_sequence,
          $.string_interpolation,
        )),
        '"""',
      ),
    ),

    _multiline_string_fragment_double: _ => choice(
      /[^"\\]+/,
      /"([^"\\]|\\")*/,
    ),

    _multiline_string_fragment_single: _ => choice(
      /[^'\\]+/,
      /'([^'\\]|\\')*/,
    ),

    // Java's character_literal requires >=1 char between quotes (single
    // chars only). Groovy reuses this token to represent its single-quoted
    // string literal (any length, including empty `''`), so override with
    // `repeat` (0+) instead of Java's `repeat1` (1+).
    character_literal: _ => token(seq(
      '\'',
      repeat(choice(/[^\\'\n]/, /\\./, /\\\n/)),
      '\'',
    )),

    _literal: ($, original) => choice(
      original,
      $.array_literal,
      $.map_literal,
    ),

    decimal_floating_point_literal: _ => token(choice(
      seq(DECIMAL_DIGITS, '.', DECIMAL_DIGITS, optional(seq((/[eE]/), optional(choice('-', '+')), DECIMAL_DIGITS)), optional(/[fFdD]/)),
      seq('.', DECIMAL_DIGITS, optional(seq((/[eE]/), optional(choice('-', '+')), DECIMAL_DIGITS)), optional(/[fFdD]/)),
      seq(DIGITS, /[eEpP]/, optional(choice('-', '+')), DECIMAL_DIGITS, optional(/[fFdD]/)),
      seq(DIGITS, optional(seq((/[eE]/), optional(choice('-', '+')), DECIMAL_DIGITS)), (/[fFdD]/)),
    )),
  },
});

/**
 * Creates a rule to match one or more of the rules separated by `separator`
 *
 * @param {RuleOrLiteral} rule
 *
 * @param {RuleOrLiteral} separator
 *
 * @returns {SeqRule}
 */
function sep1(rule, separator) {
  return seq(rule, repeat(seq(separator, rule)));
}

/**
 * Creates a rule to match one or more of the rules separated by a comma
 *
 * @param {RuleOrLiteral} rule
 *
 * @returns {SeqRule}
 */
function commaSep1(rule) {
  return seq(rule, repeat(seq(',', rule)));
}

/**
 * Creates a rule to optionally match one or more of the rules separated by a comma
 *
 * @param {RuleOrLiteral} rule
 *
 * @returns {ChoiceRule}
 */
function commaSep(rule) {
  return optional(commaSep1(rule));
}
